// -------------------------------------------------------------------------------------------------
//  Copyright (C) 2015-2026 Nautech Systems Pty Ltd. All rights reserved.
//  https://nautechsystems.io
//
//  Licensed under the GNU Lesser General Public License Version 3.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at https://www.gnu.org/licenses/lgpl-3.0.en.html
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
// -------------------------------------------------------------------------------------------------

//! Candidate-ledger Discord notifier for Alpaca scanner opportunities.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Duration, Utc};
use nautilus_alpaca::index_credit::IndexCreditConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const DEFAULT_LOOKBACK_MINUTES: i64 = 30;
const DEFAULT_DEDUPE_TTL_SECS: i64 = 3_600;
const DEFAULT_MAX_RANK: u64 = 3;
const DEFAULT_NAKED_SCORE: f64 = 95.0;
const DEFAULT_NAKED_ONE_TO_THREE_DTE_SCORE: f64 = 100.0;
const DEFAULT_IRON_CONDOR_SCORE: f64 = 80.0;
const DEFAULT_CREDIT_SCORE: f64 = 80.0;
const DEFAULT_DEBIT_SCORE: f64 = 80.0;

#[derive(Debug)]
struct Args {
    date: Option<String>,
    dry_run: bool,
    ledger_path: Option<PathBuf>,
    state_path: Option<PathBuf>,
    alerts_env_file: Option<PathBuf>,
    lookback_minutes: i64,
    dedupe_ttl_secs: i64,
    max_rank: u64,
    include_selected: bool,
    include_high_score: bool,
    include_submit_rejects: bool,
    min_score_naked: f64,
    min_score_naked_1_3dte: f64,
    min_score_iron_condor: f64,
    min_score_credit: f64,
    min_score_debit: f64,
}

#[derive(Debug)]
struct CandidateAlert {
    key: String,
    content: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct AlertState {
    sent: BTreeMap<String, String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse()?;
    load_alerts_env(args.alerts_env_file.as_deref())?;
    let config = IndexCreditConfig::from_runtime_env()?;
    let account_id = config
        .fleet_account_id
        .clone()
        .or_else(|| env::var("NAUTILUS_ALPACA_ACCOUNT").ok())
        .unwrap_or_else(|| "paper-main".to_string());
    let trade_date = args.date.clone().unwrap_or_else(|| {
        Utc::now()
            .with_timezone(&config.entry_timezone)
            .date_naive()
            .to_string()
    });
    let ledger_path = args.ledger_path.clone().unwrap_or_else(|| {
        config
            .candidate_ledger_dir
            .join(format!("{trade_date}.jsonl"))
    });
    let state_path = args
        .state_path
        .clone()
        .unwrap_or_else(|| default_alert_state_path(&config.candidate_ledger_dir, &account_id));
    let cutoff = Utc::now() - Duration::minutes(args.lookback_minutes.max(1));

    let records = read_ledger_records(&ledger_path)?;
    let mut state = load_alert_state(&state_path)?;
    prune_alert_state(&mut state, args.dedupe_ttl_secs.max(60));
    let alerts = collect_candidate_alerts(&records, &args, &account_id, cutoff);
    let pending = alerts
        .into_iter()
        .filter(|alert| !state.sent.contains_key(&alert.key))
        .collect::<Vec<_>>();

    if pending.is_empty() {
        println!(
            "candidate_alerts account={account_id} date={trade_date} ledger={} pending=0 dry_run={}",
            ledger_path.display(),
            args.dry_run,
        );
        save_alert_state(&state_path, &state)?;
        return Ok(());
    }

    if args.dry_run {
        println!(
            "candidate_alerts account={account_id} date={trade_date} ledger={} pending={} dry_run=true",
            ledger_path.display(),
            pending.len(),
        );
        for alert in &pending {
            println!("{}", alert.content);
        }
        return Ok(());
    }

    let webhook_url = discord_webhook_url()?;
    let client = reqwest::Client::new();
    for alert in pending {
        client
            .post(&webhook_url)
            .json(&json!({ "content": alert.content }))
            .send()
            .await?
            .error_for_status()?;
        state.sent.insert(alert.key, Utc::now().to_rfc3339());
    }
    save_alert_state(&state_path, &state)?;
    println!(
        "candidate_alerts account={account_id} date={trade_date} ledger={} sent=true",
        ledger_path.display(),
    );
    Ok(())
}

impl Args {
    fn parse() -> anyhow::Result<Self> {
        let mut args = Self {
            date: None,
            dry_run: true,
            ledger_path: None,
            state_path: None,
            alerts_env_file: None,
            lookback_minutes: DEFAULT_LOOKBACK_MINUTES,
            dedupe_ttl_secs: DEFAULT_DEDUPE_TTL_SECS,
            max_rank: DEFAULT_MAX_RANK,
            include_selected: true,
            include_high_score: true,
            include_submit_rejects: true,
            min_score_naked: DEFAULT_NAKED_SCORE,
            min_score_naked_1_3dte: DEFAULT_NAKED_ONE_TO_THREE_DTE_SCORE,
            min_score_iron_condor: DEFAULT_IRON_CONDOR_SCORE,
            min_score_credit: DEFAULT_CREDIT_SCORE,
            min_score_debit: DEFAULT_DEBIT_SCORE,
        };

        let mut iter = env::args().skip(1);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--date" => args.date = Some(next_arg(&mut iter, "--date")?),
                "--ledger-path" => {
                    args.ledger_path = Some(PathBuf::from(next_arg(&mut iter, "--ledger-path")?));
                }
                "--state-path" => {
                    args.state_path = Some(PathBuf::from(next_arg(&mut iter, "--state-path")?));
                }
                "--alerts-env-file" => {
                    args.alerts_env_file =
                        Some(PathBuf::from(next_arg(&mut iter, "--alerts-env-file")?));
                }
                "--lookback-minutes" => {
                    args.lookback_minutes = next_arg(&mut iter, "--lookback-minutes")?.parse()?;
                }
                "--dedupe-ttl-secs" => {
                    args.dedupe_ttl_secs = next_arg(&mut iter, "--dedupe-ttl-secs")?.parse()?;
                }
                "--max-rank" => args.max_rank = next_arg(&mut iter, "--max-rank")?.parse()?,
                "--min-score-naked" => {
                    args.min_score_naked = next_arg(&mut iter, "--min-score-naked")?.parse()?;
                }
                "--min-score-naked-1-3dte" => {
                    args.min_score_naked_1_3dte =
                        next_arg(&mut iter, "--min-score-naked-1-3dte")?.parse()?;
                }
                "--min-score-iron-condor" => {
                    args.min_score_iron_condor =
                        next_arg(&mut iter, "--min-score-iron-condor")?.parse()?;
                }
                "--min-score-credit" => {
                    args.min_score_credit = next_arg(&mut iter, "--min-score-credit")?.parse()?;
                }
                "--min-score-debit" => {
                    args.min_score_debit = next_arg(&mut iter, "--min-score-debit")?.parse()?;
                }
                "--send" => args.dry_run = false,
                "--dry-run" => args.dry_run = true,
                "--no-selected" => args.include_selected = false,
                "--no-high-score" => args.include_high_score = false,
                "--no-submit-rejects" => args.include_submit_rejects = false,
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => anyhow::bail!("unexpected argument: {other}"),
            }
        }
        Ok(args)
    }
}

fn next_arg(iter: &mut impl Iterator<Item = String>, flag: &str) -> anyhow::Result<String> {
    iter.next()
        .ok_or_else(|| anyhow::anyhow!("missing value for {flag}"))
}

fn print_usage() {
    println!(
        "usage: alpaca-candidate-alerts [--send|--dry-run] [--date YYYY-MM-DD] [--lookback-minutes N] [--max-rank N]"
    );
}

fn load_alerts_env(path: Option<&Path>) -> anyhow::Result<()> {
    let path = path
        .map(PathBuf::from)
        .or_else(|| env::var_os("NAUTILUS_ALPACA_ALERTS_ENV_FILE").map(PathBuf::from))
        .unwrap_or_else(default_alerts_env_path);
    match path.try_exists() {
        Ok(true) => {
            dotenvy::from_path_override(&path).map_err(|error| {
                anyhow::anyhow!("failed to load alerts env file {}: {error}", path.display())
            })?;
        }
        Ok(false) => {}
        Err(error) => {
            anyhow::bail!(
                "failed to inspect alerts env file {}: {error}",
                path.display()
            );
        }
    }
    Ok(())
}

fn default_alerts_env_path() -> PathBuf {
    default_config_home()
        .join("nautilus-trader")
        .join("alpaca")
        .join("alerts.env")
}

fn default_config_home() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME").map_or_else(|| home_dir().join(".config"), PathBuf::from)
}

fn default_state_home() -> PathBuf {
    env::var_os("XDG_STATE_HOME").map_or_else(|| home_dir().join(".local/state"), PathBuf::from)
}

fn home_dir() -> PathBuf {
    env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

fn default_alert_state_path(ledger_dir: &Path, account_id: &str) -> PathBuf {
    ledger_dir
        .parent()
        .map(|path| path.join("alerts"))
        .unwrap_or_else(|| {
            default_state_home()
                .join("nautilus_trader")
                .join("alpaca")
                .join(account_id)
                .join("alerts")
        })
        .join("candidate-discord-state.json")
}

fn read_ledger_records(path: &Path) -> anyhow::Result<Vec<Value>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

fn load_alert_state(path: &Path) -> anyhow::Result<AlertState> {
    if !path.exists() {
        return Ok(AlertState::default());
    }
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

fn save_alert_state(path: &Path, state: &AlertState) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

fn prune_alert_state(state: &mut AlertState, ttl_secs: i64) {
    let cutoff = Utc::now() - Duration::seconds(ttl_secs);
    state.sent.retain(|_, value| {
        DateTime::parse_from_rfc3339(value)
            .map(|ts| ts.with_timezone(&Utc) >= cutoff)
            .unwrap_or(false)
    });
}

fn collect_candidate_alerts(
    records: &[Value],
    args: &Args,
    default_account: &str,
    cutoff: DateTime<Utc>,
) -> Vec<CandidateAlert> {
    let selected = selected_candidate_signatures(records);
    let candidate_lookup = candidate_records_by_signature(records);
    let mut alerts = Vec::new();
    let mut last_selected: Option<Value> = None;
    for record in records {
        if is_selected_decision(record) {
            last_selected = Some(enriched_candidate_record(record, &candidate_lookup));
        }
        let Some(ts) = record_ts(record) else {
            continue;
        };
        if ts < cutoff {
            continue;
        }
        match record_str(record, "type") {
            Some("candidate") if args.include_high_score => {
                if selected.contains(&candidate_signature(record)) {
                    continue;
                }
                if let Some(alert) = high_score_alert(record, args, default_account) {
                    alerts.push(alert);
                }
            }
            Some("decision") if args.include_selected && is_selected_decision(record) => {
                let record = enriched_candidate_record(record, &candidate_lookup);
                alerts.push(selected_alert(&record, default_account));
            }
            Some("submit_result") if args.include_submit_rejects => {
                if record_u64(record, "rejected").unwrap_or(0) > 0
                    && let Some(decision) = &last_selected
                {
                    alerts.push(submit_rejected_alert(record, decision, default_account));
                }
            }
            _ => {}
        }
    }
    alerts
}

fn selected_candidate_signatures(records: &[Value]) -> BTreeSet<String> {
    records
        .iter()
        .filter(|record| is_selected_decision(record))
        .map(candidate_signature)
        .collect()
}

fn candidate_records_by_signature(records: &[Value]) -> BTreeMap<String, Value> {
    records
        .iter()
        .filter(|record| record_str(record, "type") == Some("candidate"))
        .map(|record| (candidate_signature(record), record.clone()))
        .collect()
}

fn enriched_candidate_record(record: &Value, candidates: &BTreeMap<String, Value>) -> Value {
    let Some(candidate) = candidates.get(&candidate_signature(record)) else {
        return record.clone();
    };
    let Value::Object(mut merged) = candidate.clone() else {
        return record.clone();
    };
    if let Value::Object(fields) = record {
        merged.extend(fields.clone());
    }
    Value::Object(merged)
}

fn high_score_alert(record: &Value, args: &Args, default_account: &str) -> Option<CandidateAlert> {
    let score = record_f64(record, "score")?;
    if record_u64(record, "rank").unwrap_or(1) > args.max_rank {
        return None;
    }
    if score < score_threshold(record, args) {
        return None;
    }
    let key = format!("candidate|{}", candidate_signature(record));
    Some(CandidateAlert {
        key,
        content: format_candidate_message("Scanner candidate", record, default_account),
    })
}

fn selected_alert(record: &Value, default_account: &str) -> CandidateAlert {
    let key = format!(
        "selected|{}|{}",
        record_str(record, "action").unwrap_or("decision"),
        candidate_signature(record)
    );
    CandidateAlert {
        key,
        content: format_candidate_message("Selected candidate", record, default_account),
    }
}

fn submit_rejected_alert(
    submit_result: &Value,
    decision: &Value,
    default_account: &str,
) -> CandidateAlert {
    let key = format!(
        "submit_rejected|{}|{}",
        record_str(decision, "order_list_id").unwrap_or("unknown_order"),
        candidate_signature(decision)
    );
    let rejected = record_u64(submit_result, "rejected").unwrap_or(0);
    CandidateAlert {
        key,
        content: format!(
            "{}\nsubmit rejected legs/orders: {}",
            format_candidate_message("Candidate submit rejected", decision, default_account),
            rejected,
        ),
    }
}

fn format_candidate_message(title: &str, record: &Value, default_account: &str) -> String {
    let account = record_str(record, "account_id").unwrap_or(default_account);
    let strategy = record_str(record, "strategy").unwrap_or("unknown_strategy");
    let underlying = record_str(record, "underlying").unwrap_or("unknown");
    let legs = candidate_legs(record);
    let score = record_f64(record, "score")
        .map(|value| format!("{value:.1}"))
        .unwrap_or_else(|| "n/a".to_string());
    let dte = candidate_dte(record)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "n/a".to_string());
    let price = record_f64(record, "credit")
        .map(|value| format!("credit ${value:.2}"))
        .or_else(|| record_f64(record, "debit").map(|value| format!("debit ${value:.2}")))
        .unwrap_or_else(|| "price n/a".to_string());
    let pop = candidate_metric(record, "breakeven_pop")
        .map(format_pct)
        .unwrap_or_else(|| "POP n/a".to_string());
    let touch = candidate_metric(record, "probability_of_touch_est")
        .map(format_pct)
        .unwrap_or_else(|| "touch n/a".to_string());
    let buying_power = record_f64(record, "estimated_buying_power_requirement")
        .or_else(|| candidate_metric(record, "estimated_buying_power_requirement"))
        .map(|value| format!("BPR ${value:.0}"))
        .unwrap_or_else(|| "BPR n/a".to_string());
    let usage = record_f64(record, "buying_power_usage_pct")
        .map(format_pct)
        .unwrap_or_else(|| "BP use n/a".to_string());
    let return_metric = record_f64(record, "return_on_buying_power")
        .map(|value| format!("RoBP {}", format_pct(value)))
        .or_else(|| {
            record_f64(record, "return_on_risk").map(|value| format!("RoR {}", format_pct(value)))
        })
        .or_else(|| record_f64(record, "reward_to_risk").map(|value| format!("R/R {:.2}", value)))
        .unwrap_or_else(|| "return n/a".to_string());

    format!(
        "**{title}** `{account}` `{strategy}`\n`{underlying}` {legs} {dte}DTE | score {score} | {price}\n{pop} | {touch} | {buying_power} | {usage} | {return_metric}"
    )
}

fn format_pct(value: f64) -> String {
    format!("{:.1}%", value * 100.0)
}

fn candidate_signature(record: &Value) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        record_str(record, "account_id").unwrap_or("unknown_account"),
        record_str(record, "trade_date").unwrap_or("unknown_date"),
        record_str(record, "strategy").unwrap_or("unknown_strategy"),
        record_str(record, "underlying").unwrap_or("unknown_underlying"),
        primary_symbol(record).unwrap_or_else(|| "unknown_symbol".to_string()),
    )
}

fn is_selected_decision(record: &Value) -> bool {
    record_str(record, "type") == Some("decision")
        && matches!(record_str(record, "action"), Some("submit" | "dry_run"))
        && record_str(record, "strategy").is_some()
}

fn score_threshold(record: &Value, args: &Args) -> f64 {
    let strategy = record_str(record, "strategy").unwrap_or_default();
    let candidate_type = record_str(record, "candidate_type").unwrap_or_default();
    if strategy.contains("naked") && strategy.contains("1_3dte") {
        args.min_score_naked_1_3dte
    } else if strategy.contains("naked") || candidate_type == "naked_option" {
        args.min_score_naked
    } else if candidate_type == "iron_condor" {
        args.min_score_iron_condor
    } else if candidate_type == "debit_spread" {
        args.min_score_debit
    } else {
        args.min_score_credit
    }
}

fn primary_symbol(record: &Value) -> Option<String> {
    for key in [
        "short_symbol",
        "long_symbol",
        "short_put_symbol",
        "short_call_symbol",
    ] {
        if let Some(value) = record_str(record, key)
            && !value.is_empty()
        {
            return Some(value.to_string());
        }
    }
    nested_str(record, &["short", "symbol"]).map(ToString::to_string)
}

fn candidate_legs(record: &Value) -> String {
    if let (Some(short_put), Some(long_put), Some(short_call), Some(long_call)) = (
        record_str(record, "short_put_symbol"),
        record_str(record, "long_put_symbol"),
        record_str(record, "short_call_symbol"),
        record_str(record, "long_call_symbol"),
    ) {
        return format!("`SP {short_put}` `LP {long_put}` `SC {short_call}` `LC {long_call}`");
    }
    if let (Some(short), Some(long)) = (
        record_str(record, "short_symbol"),
        record_str(record, "long_symbol"),
    ) && !long.is_empty()
    {
        if record_str(record, "candidate_type") == Some("debit_spread")
            || record_str(record, "strategy").is_some_and(|strategy| strategy.contains("debit"))
        {
            return format!("`L {long}` `S {short}`");
        }
        return format!("`S {short}` `L {long}`");
    }
    primary_symbol(record)
        .map(|symbol| format!("`{symbol}`"))
        .unwrap_or_else(|| "`unknown_symbol`".to_string())
}

fn candidate_dte(record: &Value) -> Option<i64> {
    record_i64(record, "dte")
        .or_else(|| nested_i64(record, &["short", "dte"]))
        .or_else(|| nested_i64(record, &["put", "short", "dte"]))
        .or_else(|| nested_i64(record, &["call", "short", "dte"]))
}

fn candidate_metric(record: &Value, key: &str) -> Option<f64> {
    record_f64(record, key)
        .or_else(|| nested_f64(record, &["short", "metrics", key]))
        .or_else(|| nested_f64(record, &["put", "short", "metrics", key]))
        .or_else(|| nested_f64(record, &["call", "short", "metrics", key]))
}

fn record_ts(record: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(record_str(record, "ts_utc")?)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn record_str<'a>(record: &'a Value, key: &str) -> Option<&'a str> {
    record.get(key)?.as_str()
}

fn nested_str<'a>(record: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut value = record;
    for key in path {
        value = value.get(*key)?;
    }
    value.as_str()
}

fn record_f64(record: &Value, key: &str) -> Option<f64> {
    record.get(key)?.as_f64()
}

fn nested_f64(record: &Value, path: &[&str]) -> Option<f64> {
    let mut value = record;
    for key in path {
        value = value.get(*key)?;
    }
    value.as_f64()
}

fn record_u64(record: &Value, key: &str) -> Option<u64> {
    record.get(key)?.as_u64()
}

fn record_i64(record: &Value, key: &str) -> Option<i64> {
    record.get(key)?.as_i64()
}

fn nested_i64(record: &Value, path: &[&str]) -> Option<i64> {
    let mut value = record;
    for key in path {
        value = value.get(*key)?;
    }
    value.as_i64()
}

fn discord_webhook_url() -> anyhow::Result<String> {
    env::var("DISCORD_WEBHOOK_URL")
        .or_else(|_| env::var("NAUTILUS_ALPACA_DISCORD_WEBHOOK_URL"))
        .map_err(|_| anyhow::anyhow!("missing DISCORD_WEBHOOK_URL for --send"))
}
