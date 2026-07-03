//! One-shot Alpaca options operator validation.

use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};

use super::{
    config_lint::{ConfigLintReport, PreRollReport, build_config_lint_report},
    operator_status, scanner_report,
};
use crate::operator;

const DEFAULT_SCANNER_SINCE_SECS: i64 = 600;

#[derive(Debug, Serialize)]
struct DoctorReport {
    checked_at_utc: String,
    health: String,
    health_reasons: Vec<String>,
    next_action: String,
    status_error: Option<String>,
    config: ConfigLintReport,
    pre_roll: Option<PreRollReport>,
    service: Option<Value>,
    runtime_runner: Option<Value>,
    fleet: Option<Value>,
    account: Option<Value>,
    event_shock: Option<Value>,
    broker: Option<Value>,
    universe: Option<Value>,
    scanner_lifecycle: Option<Value>,
    scanner: Option<Value>,
    scanner_error: Option<String>,
}

#[derive(Clone, Copy, Debug)]
struct DoctorOptions {
    json_output: bool,
    scanner_since_secs: i64,
}

pub(crate) async fn run() -> anyhow::Result<()> {
    let options = parse_args(&operator::args())?;
    let report = build_doctor_report(options).await?;

    if options.json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human_report(&report);
    }

    if report.health == "broken" {
        std::process::exit(1);
    }
    Ok(())
}

async fn build_doctor_report(options: DoctorOptions) -> anyhow::Result<DoctorReport> {
    let config = build_config_lint_report(true);
    let pre_roll = config.pre_roll.clone();
    let status_result = operator_status::collect_status_value().await;
    let scanner_result =
        scanner_report::build_scanner_report(scanner_report::ScannerReportOptions {
            since_secs: options.scanner_since_secs,
        })
        .await;

    let (status, status_error) = match status_result {
        Ok(value) => (Some(value), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let (scanner, scanner_error) = match scanner_result {
        Ok(report) => (Some(serde_json::to_value(report)?), None),
        Err(error) => (None, Some(error.to_string())),
    };

    let mut health = status
        .as_ref()
        .and_then(|status| status.get("health"))
        .and_then(Value::as_str)
        .unwrap_or("broken")
        .to_string();
    let mut health_reasons = status
        .as_ref()
        .and_then(|status| status.get("health_reasons"))
        .and_then(Value::as_array)
        .map(|values| string_array(values))
        .unwrap_or_default();
    let mut next_action = status
        .as_ref()
        .and_then(|status| status.get("next_action"))
        .and_then(Value::as_str)
        .unwrap_or("inspect_operator_status")
        .to_string();

    if let Some(error) = &status_error {
        health = "broken".to_string();
        health_reasons.push(format!("status_collect_failed: {error}"));
        next_action = "inspect_alpaca_account_config_and_broker_connectivity".to_string();
    }
    if !config.ok {
        health = "broken".to_string();
        health_reasons.push("config_lint_failed".to_string());
        if let Some(issue) = config
            .issues
            .iter()
            .find(|issue| matches!(issue.severity, "error" | "critical"))
        {
            next_action = format!("fix_{}: {}", issue.code, issue.message);
        }
    }
    if scanner_error.is_some() && health == "healthy" {
        health = "degraded".to_string();
        health_reasons.push("scanner_report_unavailable".to_string());
        next_action = "inspect_scanner_report".to_string();
    }
    if health_reasons.is_empty() {
        health_reasons.push("no_operator_alerts".to_string());
    }

    let service = status_field(status.as_ref(), "service");
    let runtime_runner = status_field(status.as_ref(), "runtime_runner");
    let account = status_field(status.as_ref(), "account");
    let event_shock = status_field(status.as_ref(), "event_shock");
    let universe = status_field(status.as_ref(), "universe");
    let scanner_lifecycle = status_field(status.as_ref(), "scanner_lifecycle");
    let broker = status.as_ref().map(|status| {
        json!({
            "orders": status.get("orders").cloned().unwrap_or(Value::Null),
            "positions": status.get("positions").cloned().unwrap_or(Value::Null),
            "spread_reconciliation_preview": status
                .get("spread_reconciliation_preview")
                .cloned()
                .unwrap_or(Value::Null),
        })
    });
    let fleet = status.as_ref().map(|status| {
        let service = status.get("service").unwrap_or(&Value::Null);
        json!({
            "fleet_account_id": service.get("fleet_account_id").cloned().unwrap_or(Value::Null),
            "strategy_profiles": service.get("strategy_profile_summaries").cloned().unwrap_or(Value::Null),
            "submitting_profiles": service.get("submitting_profiles").cloned().unwrap_or(Value::Null),
            "submitting_strategy_families": service
                .get("submitting_strategy_families")
                .cloned()
                .unwrap_or(Value::Null),
        })
    });

    Ok(DoctorReport {
        checked_at_utc: Utc::now().to_rfc3339(),
        health,
        health_reasons,
        next_action,
        status_error,
        config,
        pre_roll,
        service,
        runtime_runner,
        fleet,
        account,
        event_shock,
        broker,
        universe,
        scanner_lifecycle,
        scanner,
        scanner_error,
    })
}

fn parse_args(args: &[String]) -> anyhow::Result<DoctorOptions> {
    let mut options = DoctorOptions {
        json_output: false,
        scanner_since_secs: DEFAULT_SCANNER_SINCE_SECS,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => options.json_output = true,
            "--scanner-since-secs" | "--since-secs" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--scanner-since-secs requires a value"))?;
                options.scanner_since_secs = value.parse::<i64>()?.max(0);
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown doctor argument `{other}`"),
        }
        index += 1;
    }
    Ok(options)
}

fn status_field(status: Option<&Value>, field: &str) -> Option<Value> {
    status.and_then(|status| status.get(field).cloned())
}

fn string_array(values: &[Value]) -> Vec<String> {
    values
        .iter()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect()
}

fn print_human_report(report: &DoctorReport) {
    println!(
        "doctor: health={} next_action={} checked_at_utc={}",
        report.health, report.next_action, report.checked_at_utc
    );
    println!("health_reasons: {}", format_list(&report.health_reasons));
    println!(
        "config: ok={} issues={} universe_groups={} strategy_profiles={} submitting_profiles={}",
        report.config.ok,
        report.config.issue_count,
        report.config.universe_groups,
        report.config.strategy_profiles,
        format_list(&report.config.submitting_profiles),
    );
    if let Some(pre_roll) = &report.pre_roll {
        println!(
            "pre_roll: ok={} checks={} issues={}",
            pre_roll.ok,
            pre_roll.checks.len(),
            pre_roll.issues.len(),
        );
    }
    if let Some(error) = &report.status_error {
        println!("status_error: {error}");
    }
    if let Some(error) = &report.scanner_error {
        println!("scanner_error: {error}");
    }
    if let Some(account) = &report.account {
        println!("account: {}", compact_json(account));
    }
    if let Some(broker) = &report.broker {
        println!("broker: {}", compact_json(broker));
    }
    if let Some(runtime_runner) = &report.runtime_runner {
        println!("runtime_runner: {}", compact_json(runtime_runner));
    }
    if let Some(scanner_lifecycle) = &report.scanner_lifecycle {
        println!("scanner_lifecycle: {}", compact_json(scanner_lifecycle));
    }
    if let Some(event_shock) = &report.event_shock {
        println!("event_shock: {}", compact_json(event_shock));
    }
    if let Some(universe) = &report.universe {
        println!("universe: {}", compact_json(universe));
    }
    if let Some(scanner) = &report.scanner {
        println!("scanner: {}", compact_json(scanner));
    }
}

fn print_usage() {
    println!(
        "usage: nautilus adapters alpaca doctor [--json] [--scanner-since-secs SECS]\n\
         \n\
         Runs one-shot Alpaca options operator validation: config lint, Docker pre-roll mount\n\
         checks, live status, broker/account state, event-shock readiness, universe state, and\n\
         recent scanner quality. Use --scanner-since-secs 86400 for a daily scanner report."
    );
}

fn format_list(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values.join(",")
    }
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "unavailable".to_string())
}
