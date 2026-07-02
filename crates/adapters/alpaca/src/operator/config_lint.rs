//! Config linting and Docker pre-roll checks for the Alpaca options runtime.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use chrono::Utc;
use serde::Serialize;

use crate::{
    operator,
    options_runtime::{
        AlpacaOptionsRuntimeConfig, AlpacaOptionsStrategyFamily, AlpacaOptionsStrategyMode,
    },
};

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ConfigLintReport {
    pub checked_at_utc: String,
    pub ok: bool,
    pub issue_count: usize,
    pub issues: Vec<LintIssue>,
    pub universe_groups: usize,
    pub strategy_profiles: usize,
    pub submitting_profiles: Vec<String>,
    pub submitting_strategy_families: Vec<String>,
    pub pre_roll: Option<PreRollReport>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct LintIssue {
    pub severity: &'static str,
    pub code: &'static str,
    pub message: String,
    pub path: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PreRollReport {
    pub checked_at_utc: String,
    pub ok: bool,
    pub checks: Vec<PreRollCheck>,
    pub issues: Vec<LintIssue>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PreRollCheck {
    pub name: &'static str,
    pub env_var: Option<&'static str>,
    pub container_path: &'static str,
    pub host_source: String,
    pub source_kind: &'static str,
    pub exists: Option<bool>,
    pub container_readable: Option<bool>,
    pub mode: Option<String>,
    pub status: &'static str,
}

pub(crate) async fn run() -> anyhow::Result<()> {
    let args = operator::args();
    let options = parse_args(&args)?;
    let report = build_config_lint_report(options.pre_roll);

    if options.json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human_report(&report);
    }

    if !report.ok {
        std::process::exit(1);
    }
    Ok(())
}

pub(crate) fn build_config_lint_report(include_pre_roll: bool) -> ConfigLintReport {
    let mut issues = retired_env_var_issues();
    let mut universe_groups = 0;
    let mut strategy_profiles = 0;
    let mut submitting_profiles = Vec::new();
    let mut submitting_strategy_families = Vec::new();

    match AlpacaOptionsRuntimeConfig::from_runtime_env() {
        Ok(config) => {
            universe_groups = config.universe_groups.len();
            strategy_profiles = config.strategy_profiles.len();
            submitting_profiles = submitting_profile_ids(&config);
            submitting_strategy_families = config
                .submitting_strategy_family_names()
                .into_iter()
                .map(str::to_string)
                .collect();
            extend_config_shape_issues(&config, &mut issues);
        }
        Err(error) => {
            let message = error.to_string();
            issues.push(issue(
                "error",
                config_error_code(&message),
                message,
                env::var("ALPACA_CONFIG_PATH").ok(),
            ));
        }
    }

    let pre_roll = include_pre_roll.then(build_pre_roll_report);
    if let Some(report) = &pre_roll {
        issues.extend(report.issues.clone());
    }

    let ok = !issues.iter().any(|issue| issue_blocks_success(issue));
    ConfigLintReport {
        checked_at_utc: Utc::now().to_rfc3339(),
        ok,
        issue_count: issues.len(),
        issues,
        universe_groups,
        strategy_profiles,
        submitting_profiles,
        submitting_strategy_families,
        pre_roll,
    }
}

pub(crate) fn build_pre_roll_report() -> PreRollReport {
    if running_in_container() {
        return build_container_pre_roll_report();
    }

    let mut checks = Vec::new();
    let mut issues = Vec::new();
    for mount in docker_mount_specs() {
        let check = check_mount(mount);
        if check.status == "missing" {
            issues.push(issue(
                "error",
                "docker_mount_missing",
                format!(
                    "{} host source {} does not exist",
                    check.name, check.host_source
                ),
                Some(check.host_source.clone()),
            ));
        } else if check.status == "not_container_readable" {
            issues.push(issue(
                "error",
                "docker_mount_not_container_readable",
                format!(
                    "{} host source {} is not readable by the non-root container user",
                    check.name, check.host_source
                ),
                Some(check.host_source.clone()),
            ));
        }
        checks.push(check);
    }

    let ok = issues.is_empty();
    PreRollReport {
        checked_at_utc: Utc::now().to_rfc3339(),
        ok,
        checks,
        issues,
    }
}

fn build_container_pre_roll_report() -> PreRollReport {
    let mut checks = Vec::new();
    let mut issues = Vec::new();
    for mount in container_mount_specs() {
        let check = check_container_mount(mount);
        if check.status == "missing" {
            issues.push(issue(
                "error",
                "container_mount_missing",
                format!(
                    "{} container path {} does not exist",
                    check.name, check.container_path
                ),
                Some(check.container_path.to_string()),
            ));
        } else if check.status == "not_container_readable" {
            issues.push(issue(
                "error",
                "container_mount_not_readable",
                format!(
                    "{} container path {} is not readable by the runtime user",
                    check.name, check.container_path
                ),
                Some(check.container_path.to_string()),
            ));
        }
        checks.push(check);
    }

    let ok = issues.is_empty();
    PreRollReport {
        checked_at_utc: Utc::now().to_rfc3339(),
        ok,
        checks,
        issues,
    }
}

#[derive(Clone, Copy, Debug)]
struct Options {
    json_output: bool,
    pre_roll: bool,
}

fn parse_args(args: &[String]) -> anyhow::Result<Options> {
    let mut options = Options {
        json_output: false,
        pre_roll: false,
    };
    for arg in args {
        match arg.as_str() {
            "--json" => options.json_output = true,
            "--pre-roll" => options.pre_roll = true,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown config lint argument `{other}`"),
        }
    }
    Ok(options)
}

fn extend_config_shape_issues(config: &AlpacaOptionsRuntimeConfig, issues: &mut Vec<LintIssue>) {
    if config.universe_groups.is_empty() {
        issues.push(issue(
            "warning",
            "no_universe_groups_configured",
            "no reusable universe_groups are configured; profiles can still use direct underlyings, but shared static groups are harder to audit".to_string(),
            Some("universe_groups".to_string()),
        ));
    }

    if config.open_orders_enabled && submitting_profile_ids(config).is_empty() {
        issues.push(issue(
            "warning",
            "open_orders_enabled_without_submitting_profiles",
            "ALPACA_OPEN_ORDERS is enabled but no strategy profile is in live mode".to_string(),
            Some("runtime.open_orders".to_string()),
        ));
    }

    for profile in &config.strategy_profiles {
        if profile.universe_groups.is_empty() && !profile.include_underlyings.is_empty() {
            issues.push(issue(
                "info",
                "profile_uses_direct_underlyings",
                format!(
                    "profile {} resolves from direct underlyings only; consider a named universe_group when this list should be reused",
                    profile.id
                ),
                Some(format!("strategies.{}", profile.id)),
            ));
        }
        if is_undefined_risk_family(profile.family)
            && matches!(profile.mode, AlpacaOptionsStrategyMode::Live)
        {
            issues.push(issue(
                "warning",
                "live_undefined_risk_profile",
                format!(
                    "profile {} is live undefined-risk; keep this isolated behind a reviewed account/profile policy",
                    profile.id
                ),
                Some(format!("strategies.{}", profile.id)),
            ));
        }
        if profile.quantity == 0 {
            issues.push(issue(
                "error",
                "profile_quantity_zero",
                format!("profile {} has zero quantity", profile.id),
                Some(format!("strategies.{}.quantity", profile.id)),
            ));
        }
    }

    for block in &config.fleet_policy_blocks {
        issues.push(issue(
            "error",
            "fleet_policy_block",
            format!("fleet policy blocks this runtime: {block}"),
            Some("fleet".to_string()),
        ));
    }

    if config.event_shock.required && config.event_shock.dry_run_only {
        issues.push(issue(
            "error",
            "event_shock_required_unavailable",
            format!(
                "required event-shock source is unavailable or stale: source={} freshness={} reason={}",
                config.event_shock.source,
                config.event_shock.freshness,
                config
                    .event_shock
                    .unavailable_reason
                    .as_deref()
                    .unwrap_or("none"),
            ),
            Some("event_shock".to_string()),
        ));
    } else if config.event_shock.dry_run_only {
        issues.push(issue(
            "warning",
            "event_shock_dry_run_only",
            format!(
                "event-shock is dry-run-only: source={} freshness={} reason={}",
                config.event_shock.source,
                config.event_shock.freshness,
                config
                    .event_shock
                    .unavailable_reason
                    .as_deref()
                    .unwrap_or("none"),
            ),
            Some("event_shock".to_string()),
        ));
    }
}

fn retired_env_var_issues() -> Vec<LintIssue> {
    let mut issues = Vec::new();
    let retired_order_vars = [
        "ALPACA_SUBMIT",
        "ALPACA_MANAGE",
        "ALPACA_CLOSE",
        "ALPACA_KILL_SWITCH",
        "ALPACA_OPTIONS_LIVE_ENTRY_SUBMIT_ENABLED",
    ]
    .into_iter()
    .filter(|name| env::var_os(name).is_some())
    .collect::<Vec<_>>();
    if !retired_order_vars.is_empty() {
        issues.push(issue(
            "error",
            "retired_order_capability_env_vars",
            format!(
                "retired order capability env vars are set: {}; use ALPACA_OPEN_ORDERS and ALPACA_CLOSE_ORDERS",
                retired_order_vars.join(", ")
            ),
            None,
        ));
    }

    let retired_strategy_vars = [
        "ALPACA_STRATEGY_FAMILIES",
        "ALPACA_DRY_RUN_FAMILIES",
        "ALPACA_QTY",
    ]
    .into_iter()
    .filter(|name| env::var_os(name).is_some())
    .collect::<Vec<_>>();
    if !retired_strategy_vars.is_empty() {
        issues.push(issue(
            "error",
            "retired_strategy_composition_env_vars",
            format!(
                "retired strategy composition env vars are set: {}; configure [[strategies]] blocks instead",
                retired_strategy_vars.join(", ")
            ),
            None,
        ));
    }
    issues
}

fn submitting_profile_ids(config: &AlpacaOptionsRuntimeConfig) -> Vec<String> {
    if !config.open_orders_enabled {
        return Vec::new();
    }
    config
        .strategy_profiles
        .iter()
        .filter(|profile| matches!(profile.mode, AlpacaOptionsStrategyMode::Live))
        .map(|profile| profile.id.clone())
        .collect()
}

fn config_error_code(message: &str) -> &'static str {
    if message.contains("retired Alpaca order capability env vars") {
        "retired_order_capability_env_vars"
    } else if message.contains("retired Alpaca strategy composition env vars") {
        "retired_strategy_composition_env_vars"
    } else if message.contains("positional Alpaca underlyings are retired") {
        "retired_positional_underlyings"
    } else if message.contains("references unknown universe group") {
        "unknown_universe_group"
    } else if message.contains("requires universe_groups, underlyings, or include_underlyings") {
        "profile_missing_universe"
    } else if message.contains("requires at least one explicit [[strategies]] profile") {
        "missing_strategy_profiles"
    } else if message.contains("unknown field") {
        "unknown_config_field"
    } else {
        "runtime_config_invalid"
    }
}

fn is_undefined_risk_family(family: AlpacaOptionsStrategyFamily) -> bool {
    matches!(
        family,
        AlpacaOptionsStrategyFamily::NakedPut
            | AlpacaOptionsStrategyFamily::NakedCall
            | AlpacaOptionsStrategyFamily::NakedPutOneToThreeDte
            | AlpacaOptionsStrategyFamily::NakedCallOneToThreeDte
    )
}

#[derive(Clone, Debug)]
struct DockerMountSpec {
    name: &'static str,
    env_var: Option<&'static str>,
    default_source: MountDefault,
    container_path: &'static str,
    expect_dir: bool,
}

#[derive(Clone, Debug)]
struct ContainerMountSpec {
    name: &'static str,
    container_path: &'static str,
    expect_dir: bool,
}

#[derive(Clone, Debug)]
enum MountDefault {
    NamedVolume(&'static str),
    ComposeFile(&'static str),
}

fn container_mount_specs() -> Vec<ContainerMountSpec> {
    vec![
        ContainerMountSpec {
            name: "scheduled_events",
            container_path: "/state/scheduled_events",
            expect_dir: true,
        },
        ContainerMountSpec {
            name: "earnings",
            container_path: "/state/earnings",
            expect_dir: true,
        },
        ContainerMountSpec {
            name: "fleet_config",
            container_path: "/config/fleet.toml",
            expect_dir: false,
        },
        ContainerMountSpec {
            name: "base_config",
            container_path: "/config/base-options.toml",
            expect_dir: false,
        },
        ContainerMountSpec {
            name: "base_engine_config",
            container_path: "/config/base-options-engine.toml",
            expect_dir: false,
        },
        ContainerMountSpec {
            name: "runtime_config",
            container_path: "/config/options.toml",
            expect_dir: false,
        },
    ]
}

fn docker_mount_specs() -> Vec<DockerMountSpec> {
    vec![
        DockerMountSpec {
            name: "scheduled_events",
            env_var: Some("NAUTILUS_ALPACA_DOCKER_SCHEDULED_EVENT_ROOT"),
            default_source: MountDefault::NamedVolume("alpaca-scheduled-events"),
            container_path: "/state/scheduled_events",
            expect_dir: true,
        },
        DockerMountSpec {
            name: "earnings",
            env_var: Some("NAUTILUS_ALPACA_DOCKER_EARNINGS_DIR"),
            default_source: MountDefault::NamedVolume("alpaca-earnings"),
            container_path: "/state/earnings",
            expect_dir: true,
        },
        DockerMountSpec {
            name: "fleet_config",
            env_var: Some("NAUTILUS_ALPACA_DOCKER_FLEET_CONFIG"),
            default_source: MountDefault::ComposeFile("alpaca-fleet.toml.example"),
            container_path: "/config/fleet.toml",
            expect_dir: false,
        },
        DockerMountSpec {
            name: "base_config",
            env_var: Some("NAUTILUS_ALPACA_DOCKER_BASE_CONFIG"),
            default_source: MountDefault::ComposeFile("alpaca-options.base.toml.example"),
            container_path: "/config/base-options.toml,/config/base-options-engine.toml",
            expect_dir: false,
        },
        DockerMountSpec {
            name: "runtime_config",
            env_var: Some("NAUTILUS_ALPACA_DOCKER_CONFIG"),
            default_source: MountDefault::ComposeFile("alpaca-options.toml.example"),
            container_path: "/config/options.toml",
            expect_dir: false,
        },
    ]
}

fn check_container_mount(spec: ContainerMountSpec) -> PreRollCheck {
    let path = PathBuf::from(spec.container_path);
    let metadata = fs::metadata(&path);
    let exists = metadata.is_ok();
    let container_readable = metadata.as_ref().ok().map(|metadata| {
        metadata_shape_matches(metadata, spec.expect_dir) && metadata_container_readable(metadata)
    });
    let status = if !exists {
        "missing"
    } else if container_readable == Some(false) {
        "not_container_readable"
    } else {
        "ok"
    };
    PreRollCheck {
        name: spec.name,
        env_var: None,
        container_path: spec.container_path,
        host_source: spec.container_path.to_string(),
        source_kind: "container_path",
        exists: Some(exists),
        container_readable,
        mode: metadata.as_ref().ok().and_then(metadata_mode),
        status,
    }
}

fn check_mount(spec: DockerMountSpec) -> PreRollCheck {
    let raw_source = spec
        .env_var
        .and_then(|name| env::var(name).ok())
        .filter(|value| !value.trim().is_empty());
    let source = raw_source
        .as_deref()
        .map(MountSource::from_value)
        .unwrap_or_else(|| default_mount_source(&spec.default_source));

    match source {
        MountSource::NamedVolume(name) => PreRollCheck {
            name: spec.name,
            env_var: spec.env_var,
            container_path: spec.container_path,
            host_source: name,
            source_kind: "named_volume",
            exists: None,
            container_readable: None,
            mode: None,
            status: "ok",
        },
        MountSource::Bind(path) => {
            let metadata = fs::metadata(&path);
            let exists = metadata.is_ok();
            let container_readable = metadata.as_ref().ok().map(|metadata| {
                metadata_shape_matches(metadata, spec.expect_dir)
                    && metadata_container_readable(metadata)
            });
            let status = if !exists {
                "missing"
            } else if container_readable == Some(false) {
                "not_container_readable"
            } else {
                "ok"
            };
            PreRollCheck {
                name: spec.name,
                env_var: spec.env_var,
                container_path: spec.container_path,
                host_source: path.display().to_string(),
                source_kind: "bind",
                exists: Some(exists),
                container_readable,
                mode: metadata.as_ref().ok().and_then(metadata_mode),
                status,
            }
        }
    }
}

fn running_in_container() -> bool {
    Path::new("/.dockerenv").exists()
        || env::var("NAUTILUS_ALPACA_SERVICE").as_deref() == Ok("alpaca-options-container")
}

#[derive(Clone, Debug)]
enum MountSource {
    NamedVolume(String),
    Bind(PathBuf),
}

impl MountSource {
    fn from_value(value: &str) -> Self {
        if looks_like_bind_source(value) {
            Self::Bind(resolve_compose_path(value))
        } else {
            Self::NamedVolume(value.to_string())
        }
    }
}

fn default_mount_source(default: &MountDefault) -> MountSource {
    match default {
        MountDefault::NamedVolume(name) => MountSource::NamedVolume((*name).to_string()),
        MountDefault::ComposeFile(file_name) => MountSource::Bind(compose_dir().join(file_name)),
    }
}

fn looks_like_bind_source(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with("./")
        || value.starts_with("../")
        || value.contains('/')
}

fn resolve_compose_path(value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        compose_dir().join(path)
    }
}

fn compose_dir() -> PathBuf {
    repo_root().join("deploy/alpaca")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn metadata_shape_matches(metadata: &fs::Metadata, expect_dir: bool) -> bool {
    if expect_dir {
        metadata.is_dir()
    } else {
        metadata.is_file()
    }
}

#[cfg(unix)]
fn metadata_container_readable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let mode = metadata.permissions().mode();
    if metadata.is_dir() {
        mode & 0o005 == 0o005
    } else {
        mode & 0o004 == 0o004
    }
}

#[cfg(not(unix))]
fn metadata_container_readable(metadata: &fs::Metadata) -> bool {
    !metadata.permissions().readonly()
}

#[cfg(unix)]
fn metadata_mode(metadata: &fs::Metadata) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;

    Some(format!("{:o}", metadata.permissions().mode() & 0o777))
}

#[cfg(not(unix))]
fn metadata_mode(_metadata: &fs::Metadata) -> Option<String> {
    None
}

fn issue(
    severity: &'static str,
    code: &'static str,
    message: String,
    path: Option<String>,
) -> LintIssue {
    LintIssue {
        severity,
        code,
        message,
        path,
    }
}

fn issue_blocks_success(issue: &LintIssue) -> bool {
    matches!(issue.severity, "error" | "critical")
}

fn print_human_report(report: &ConfigLintReport) {
    println!(
        "config_lint: ok={} issues={} universe_groups={} strategy_profiles={} submitting_profiles={}",
        report.ok,
        report.issue_count,
        report.universe_groups,
        report.strategy_profiles,
        format_list(&report.submitting_profiles),
    );
    for issue in &report.issues {
        println!(
            "issue: severity={} code={} path={} message={}",
            issue.severity,
            issue.code,
            issue.path.as_deref().unwrap_or("none"),
            issue.message,
        );
    }
    if let Some(pre_roll) = &report.pre_roll {
        println!(
            "pre_roll: ok={} checks={} issues={}",
            pre_roll.ok,
            pre_roll.checks.len(),
            pre_roll.issues.len(),
        );
        for check in &pre_roll.checks {
            println!(
                "pre_roll_check: name={} source_kind={} host_source={} container_path={} exists={} container_readable={} mode={} status={}",
                check.name,
                check.source_kind,
                check.host_source,
                check.container_path,
                check
                    .exists
                    .map_or_else(|| "n/a".to_string(), |value| value.to_string()),
                check
                    .container_readable
                    .map_or_else(|| "n/a".to_string(), |value| value.to_string()),
                check.mode.as_deref().unwrap_or("n/a"),
                check.status,
            );
        }
    }
}

fn print_usage() {
    println!(
        "usage: nautilus adapters alpaca config lint [--json] [--pre-roll]\n\
         \n\
         Checks the Alpaca options runtime config for retired surfaces, profile/universe shape,\n\
         event-shock readiness, fleet policy blocks, and optional Docker pre-roll mounts."
    );
}

fn format_list(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values.join(",")
    }
}
