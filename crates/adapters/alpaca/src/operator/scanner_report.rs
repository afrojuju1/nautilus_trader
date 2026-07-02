//! Scanner quality reports from Alpaca option-chain operator evidence.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use nautilus_infrastructure::sql::operational::{
    CandidateLedgerSummaryFilters, read_candidate_ledger_records,
};
use serde::Serialize;
use serde_json::Value;

use crate::{operator, options_runtime::AlpacaOptionsRuntimeConfig, runtime::read_operator_events};

const DEFAULT_SINCE_SECS: i64 = 86_400;

#[derive(Clone, Copy, Debug)]
pub(crate) struct ScannerReportOptions {
    pub since_secs: i64,
}

impl Default for ScannerReportOptions {
    fn default() -> Self {
        Self {
            since_secs: DEFAULT_SINCE_SECS,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ScannerQualityReport {
    checked_at_utc: String,
    since_secs: i64,
    log_file: String,
    log_file_exists: bool,
    scan_events: usize,
    operator_event_scan_events: usize,
    candidate_ledger_scan_records: usize,
    candidate_ledger_error: Option<String>,
    total_scans: usize,
    candidate_scans: usize,
    no_candidate_scans: usize,
    ranked_entries: usize,
    candidate_count: usize,
    scoreable_count: usize,
    contract_count: usize,
    snapshot_count: usize,
    rejection_counts: BTreeMap<String, usize>,
    reason_counts: BTreeMap<String, usize>,
    profile_summaries: Vec<ScannerProfileSummary>,
    underlying_summaries: Vec<ScannerUnderlyingSummary>,
    latest_scan: Option<Value>,
}

#[derive(Debug, Serialize)]
struct ScannerProfileSummary {
    profile_id: String,
    profile_family: String,
    profile_mode: String,
    scans: usize,
    underlyings: usize,
    candidate_scans: usize,
    no_candidate_scans: usize,
    candidate_count: usize,
    scoreable_count: usize,
    rejection_counts: BTreeMap<String, usize>,
    reason_counts: BTreeMap<String, usize>,
    latest_ts_utc: Option<String>,
}

#[derive(Debug, Serialize)]
struct ScannerUnderlyingSummary {
    underlying: String,
    scans: usize,
    profile_count: usize,
    candidate_scans: usize,
    no_candidate_scans: usize,
    candidate_count: usize,
    scoreable_count: usize,
    rejection_counts: BTreeMap<String, usize>,
    reason_counts: BTreeMap<String, usize>,
    latest_ts_utc: Option<String>,
}

#[derive(Default)]
struct ScannerAggregate {
    scans: usize,
    candidate_scans: usize,
    no_candidate_scans: usize,
    ranked_entries: usize,
    candidate_count: usize,
    scoreable_count: usize,
    contract_count: usize,
    snapshot_count: usize,
    rejection_counts: BTreeMap<String, usize>,
    reason_counts: BTreeMap<String, usize>,
    latest_ts_utc: Option<String>,
    underlyings: BTreeMap<String, usize>,
    profiles: BTreeMap<String, usize>,
    profile_family: Option<String>,
    profile_mode: Option<String>,
}

pub(crate) async fn run() -> anyhow::Result<()> {
    let args = operator::args();
    let parsed = parse_args(&args)?;
    let report = build_scanner_report(ScannerReportOptions {
        since_secs: parsed.since_secs,
    })
    .await?;

    if parsed.json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human_report(&report);
    }
    Ok(())
}

pub(crate) async fn build_scanner_report(
    options: ScannerReportOptions,
) -> anyhow::Result<ScannerQualityReport> {
    let config = AlpacaOptionsRuntimeConfig::from_runtime_env()?;
    let log_path = operator_log_path(&config);
    let events = read_operator_events(&log_path);
    let (candidate_ledger_records, candidate_ledger_error) =
        load_candidate_ledger_records(options).await;
    Ok(build_report_from_events(
        &log_path,
        &events,
        &candidate_ledger_records,
        candidate_ledger_error,
        options,
    ))
}

fn build_report_from_events(
    log_path: &Path,
    events: &[Value],
    candidate_ledger_records: &[Value],
    candidate_ledger_error: Option<String>,
    options: ScannerReportOptions,
) -> ScannerQualityReport {
    let now = Utc::now();
    let mut total = ScannerAggregate::default();
    let mut profiles = BTreeMap::<String, ScannerAggregate>::new();
    let mut underlyings = BTreeMap::<String, ScannerAggregate>::new();
    let mut seen_evidence = BTreeSet::<String>::new();
    let mut scan_events = 0;
    let mut operator_event_scan_events = 0;
    let mut candidate_ledger_scan_records = 0;
    let mut latest_scan = None;

    for event in events
        .iter()
        .filter(|event| scanner_event_is_in_window(event, now, options))
    {
        if !seen_evidence.insert(evidence_key(event)) {
            continue;
        }
        operator_event_scan_events += 1;
        apply_scan_evidence(
            event,
            &mut total,
            &mut profiles,
            &mut underlyings,
            &mut scan_events,
            &mut latest_scan,
        );
    }

    for record in candidate_ledger_records
        .iter()
        .filter(|record| scanner_record_is_in_window(record, now, options))
    {
        if !seen_evidence.insert(evidence_key(record)) {
            continue;
        }
        candidate_ledger_scan_records += 1;
        apply_scan_evidence(
            record,
            &mut total,
            &mut profiles,
            &mut underlyings,
            &mut scan_events,
            &mut latest_scan,
        );
    }

    ScannerQualityReport {
        checked_at_utc: now.to_rfc3339(),
        since_secs: options.since_secs,
        log_file: log_path.display().to_string(),
        log_file_exists: log_path.exists(),
        scan_events,
        operator_event_scan_events,
        candidate_ledger_scan_records,
        candidate_ledger_error,
        total_scans: total.scans,
        candidate_scans: total.candidate_scans,
        no_candidate_scans: total.no_candidate_scans,
        ranked_entries: total.ranked_entries,
        candidate_count: total.candidate_count,
        scoreable_count: total.scoreable_count,
        contract_count: total.contract_count,
        snapshot_count: total.snapshot_count,
        rejection_counts: total.rejection_counts,
        reason_counts: total.reason_counts,
        profile_summaries: profiles
            .into_iter()
            .map(|(profile_id, aggregate)| ScannerProfileSummary {
                profile_id,
                profile_family: aggregate
                    .profile_family
                    .unwrap_or_else(|| "unknown".to_string()),
                profile_mode: aggregate
                    .profile_mode
                    .unwrap_or_else(|| "unknown".to_string()),
                scans: aggregate.scans,
                underlyings: aggregate.underlyings.len(),
                candidate_scans: aggregate.candidate_scans,
                no_candidate_scans: aggregate.no_candidate_scans,
                candidate_count: aggregate.candidate_count,
                scoreable_count: aggregate.scoreable_count,
                rejection_counts: aggregate.rejection_counts,
                reason_counts: aggregate.reason_counts,
                latest_ts_utc: aggregate.latest_ts_utc,
            })
            .collect(),
        underlying_summaries: underlyings
            .into_iter()
            .map(|(underlying, aggregate)| ScannerUnderlyingSummary {
                underlying,
                scans: aggregate.scans,
                profile_count: aggregate.profiles.len(),
                candidate_scans: aggregate.candidate_scans,
                no_candidate_scans: aggregate.no_candidate_scans,
                candidate_count: aggregate.candidate_count,
                scoreable_count: aggregate.scoreable_count,
                rejection_counts: aggregate.rejection_counts,
                reason_counts: aggregate.reason_counts,
                latest_ts_utc: aggregate.latest_ts_utc,
            })
            .collect(),
        latest_scan,
    }
}

fn apply_scan_evidence(
    evidence: &Value,
    total: &mut ScannerAggregate,
    profiles: &mut BTreeMap<String, ScannerAggregate>,
    underlyings: &mut BTreeMap<String, ScannerAggregate>,
    scan_events: &mut usize,
    latest_scan: &mut Option<Value>,
) {
    *scan_events += 1;
    *latest_scan = Some(evidence.clone());
    total.ranked_entries += value_usize(evidence, "ranked_entries").unwrap_or_default();
    let event_ts = evidence
        .get("ts_utc")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let Some(scans) = evidence.get("scans").and_then(Value::as_array) else {
        return;
    };
    for scan in scans {
        let profile_id = scan
            .get("profile_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let underlying = scan
            .get("underlying")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        apply_scan(total, scan, event_ts.as_deref());

        let profile = profiles.entry(profile_id.clone()).or_default();
        profile.profile_family = scan
            .get("profile_family")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or_else(|| {
                scan.get("strategy")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .or_else(|| profile.profile_family.clone());
        profile.profile_mode = scan
            .get("profile_mode")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or_else(|| profile.profile_mode.clone());
        profile.underlyings.insert(underlying.clone(), 1);
        apply_scan(profile, scan, event_ts.as_deref());

        let underlying_summary = underlyings.entry(underlying).or_default();
        underlying_summary.profiles.insert(profile_id, 1);
        apply_scan(underlying_summary, scan, event_ts.as_deref());
    }
}

impl ScannerAggregate {
    fn merge_rejections(&mut self, scan: &Value) {
        let Some(rejections) = scan.get("rejections").and_then(Value::as_object) else {
            return;
        };
        for (reason, count) in rejections {
            *self.rejection_counts.entry(reason.clone()).or_default() +=
                count.as_u64().unwrap_or_default() as usize;
        }
    }
}

fn apply_scan(aggregate: &mut ScannerAggregate, scan: &Value, event_ts: Option<&str>) {
    aggregate.scans += 1;
    let candidate_count = value_usize(scan, "candidate_count").unwrap_or_default();
    aggregate.candidate_count += candidate_count;
    aggregate.contract_count += value_usize(scan, "contracts").unwrap_or_default();
    aggregate.snapshot_count += value_usize(scan, "snapshots").unwrap_or_default();
    aggregate.scoreable_count += value_usize(scan, "scoreable").unwrap_or_default();
    if scan.get("outcome").and_then(Value::as_str) == Some("candidate") || candidate_count > 0 {
        aggregate.candidate_scans += 1;
    } else {
        aggregate.no_candidate_scans += 1;
        let reason = scan
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("no_candidate");
        *aggregate
            .reason_counts
            .entry(reason.to_string())
            .or_default() += 1;
    }
    aggregate.merge_rejections(scan);
    if let Some(event_ts) = event_ts {
        aggregate.latest_ts_utc = Some(event_ts.to_string());
    }
}

#[derive(Clone, Copy, Debug)]
struct ParsedArgs {
    json_output: bool,
    since_secs: i64,
}

fn parse_args(args: &[String]) -> anyhow::Result<ParsedArgs> {
    let mut parsed = ParsedArgs {
        json_output: false,
        since_secs: DEFAULT_SINCE_SECS,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => parsed.json_output = true,
            "--since-secs" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--since-secs requires a value"))?;
                parsed.since_secs = value.parse::<i64>()?.max(0);
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown scanner report argument `{other}`"),
        }
        index += 1;
    }
    Ok(parsed)
}

fn scanner_event_is_in_window(
    event: &Value,
    now: DateTime<Utc>,
    options: ScannerReportOptions,
) -> bool {
    if event.get("type").and_then(Value::as_str) != Some("option_chain_candidate_scan") {
        return false;
    }
    if options.since_secs == 0 {
        return true;
    }
    let Some(ts_utc) = event.get("ts_utc").and_then(Value::as_str) else {
        return true;
    };
    DateTime::parse_from_rfc3339(ts_utc)
        .map(|timestamp| {
            now - timestamp.with_timezone(&Utc) <= Duration::seconds(options.since_secs)
        })
        .unwrap_or(true)
}

fn scanner_record_is_in_window(
    record: &Value,
    now: DateTime<Utc>,
    options: ScannerReportOptions,
) -> bool {
    if record.get("type").and_then(Value::as_str) != Some("scanner_result") {
        return false;
    }
    record_is_in_window(record, now, options)
}

fn record_is_in_window(record: &Value, now: DateTime<Utc>, options: ScannerReportOptions) -> bool {
    if options.since_secs == 0 {
        return true;
    }
    let Some(ts_utc) = record.get("ts_utc").and_then(Value::as_str) else {
        return true;
    };
    DateTime::parse_from_rfc3339(ts_utc)
        .map(|timestamp| {
            now - timestamp.with_timezone(&Utc) <= Duration::seconds(options.since_secs)
        })
        .unwrap_or(true)
}

async fn load_candidate_ledger_records(
    options: ScannerReportOptions,
) -> (Vec<Value>, Option<String>) {
    if env::var_os("NAUTILUS_OPERATIONAL_DATABASE_URL").is_none() {
        return (Vec::new(), None);
    }

    let config =
        match AlpacaOptionsRuntimeConfig::from_runtime_env_with_read_only_operational_store().await
        {
            Ok(config) => config,
            Err(error) => return (Vec::new(), Some(error.to_string())),
        };
    let Some(repository) = &config.operational_repository else {
        return (Vec::new(), None);
    };
    let filters = CandidateLedgerSummaryFilters {
        since: ledger_since_date(options),
        until: None,
    };
    match read_candidate_ledger_records(repository, config.operational_account_id(), filters).await
    {
        Ok(records) => (records, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    }
}

fn ledger_since_date(options: ScannerReportOptions) -> Option<NaiveDate> {
    if options.since_secs == 0 {
        return None;
    }
    let days = ((options.since_secs + 86_399) / 86_400).max(1);
    Utc::now()
        .date_naive()
        .checked_sub_signed(Duration::days(days))
}

fn evidence_key(evidence: &Value) -> String {
    let source = evidence
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let ts_utc = evidence
        .get("ts_utc")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let series_id = evidence
        .get("series_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let underlying = evidence
        .get("underlying")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    format!("{source}|{ts_utc}|{series_id}|{underlying}")
}

fn operator_log_path(config: &AlpacaOptionsRuntimeConfig) -> PathBuf {
    let state_home = env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".local/state"));
    let default_state_dir = state_home.join("nautilus_trader");
    let fleet_log_dir = config.fleet.as_ref().and_then(|fleet| {
        fleet
            .current_account()
            .and_then(|account| fleet.log_dir(account))
    });
    let log_dir = env::var("NAUTILUS_ALPACA_LOG_DIR")
        .map(PathBuf::from)
        .ok()
        .or(fleet_log_dir)
        .unwrap_or_else(|| default_state_dir.join("logs"));
    log_dir.join("alpaca-options.log")
}

fn home_dir() -> PathBuf {
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn value_usize(value: &Value, key: &str) -> Option<usize> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn print_human_report(report: &ScannerQualityReport) {
    println!(
        "scanner_report: since_secs={} log={} log_exists={} scan_events={} operator_events={} candidate_ledger_records={} total_scans={} candidate_scans={} no_candidate_scans={} ranked_entries={} candidate_count={} scoreable_count={}",
        report.since_secs,
        report.log_file,
        report.log_file_exists,
        report.scan_events,
        report.operator_event_scan_events,
        report.candidate_ledger_scan_records,
        report.total_scans,
        report.candidate_scans,
        report.no_candidate_scans,
        report.ranked_entries,
        report.candidate_count,
        report.scoreable_count,
    );
    if let Some(error) = &report.candidate_ledger_error {
        println!("candidate_ledger_error: {error}");
    }
    println!(
        "scanner_rejections: {}",
        format_counts(&report.rejection_counts)
    );
    println!("scanner_reasons: {}", format_counts(&report.reason_counts));
    for profile in &report.profile_summaries {
        println!(
            "scanner_profile: profile_id={} family={} mode={} scans={} underlyings={} candidate_scans={} no_candidate_scans={} candidate_count={} scoreable_count={} rejections={} reasons={} latest_ts_utc={}",
            profile.profile_id,
            profile.profile_family,
            profile.profile_mode,
            profile.scans,
            profile.underlyings,
            profile.candidate_scans,
            profile.no_candidate_scans,
            profile.candidate_count,
            profile.scoreable_count,
            format_counts(&profile.rejection_counts),
            format_counts(&profile.reason_counts),
            profile.latest_ts_utc.as_deref().unwrap_or("none"),
        );
    }
    for underlying in &report.underlying_summaries {
        println!(
            "scanner_underlying: underlying={} scans={} profiles={} candidate_scans={} no_candidate_scans={} candidate_count={} scoreable_count={} rejections={} reasons={} latest_ts_utc={}",
            underlying.underlying,
            underlying.scans,
            underlying.profile_count,
            underlying.candidate_scans,
            underlying.no_candidate_scans,
            underlying.candidate_count,
            underlying.scoreable_count,
            format_counts(&underlying.rejection_counts),
            format_counts(&underlying.reason_counts),
            underlying.latest_ts_utc.as_deref().unwrap_or("none"),
        );
    }
}

fn print_usage() {
    println!(
        "usage: nautilus adapters alpaca scanner report [--json] [--since-secs SECS]\n\
         \n\
         Summarizes recent option-chain scanner evidence by profile, underlying, rejection reason,\n\
         and no-candidate reason. Use --since-secs 0 to read all operator events in the log."
    );
}

fn format_counts(counts: &BTreeMap<String, usize>) -> String {
    if counts.is_empty() {
        return "none".to_string();
    }
    counts
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",")
}
