//! Resolved universe planning diagnostics for Alpaca options profiles.

use chrono::Utc;
use serde::Serialize;

use crate::{
    candidate_scan_actor::option_universe_intents_from_strategy_profiles,
    operator,
    options_runtime::{
        AlpacaOptionsRuntimeConfig, AlpacaOptionsStrategyFamily, AlpacaOptionsStrategyMode,
    },
};

#[derive(Debug, Serialize)]
struct UniversePlan {
    checked_at_utc: String,
    summary: UniversePlanSummary,
    universe_groups: Vec<UniverseGroupPlan>,
    strategy_profiles: Vec<StrategyProfilePlan>,
    universe_intents: Vec<UniverseIntentPlan>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct UniversePlanSummary {
    universe_groups: usize,
    strategy_profiles: usize,
    unique_underlyings: usize,
    resolved_underlyings: usize,
    universe_intents: usize,
}

#[derive(Debug, Serialize)]
struct UniverseGroupPlan {
    name: String,
    member_count: usize,
    members: Vec<String>,
}

#[derive(Debug, Serialize)]
struct StrategyProfilePlan {
    id: String,
    family: String,
    mode: String,
    quantity: u64,
    universe_groups: Vec<String>,
    include_underlyings: Vec<String>,
    exclude_underlyings: Vec<String>,
    resolved_underlying_count: usize,
    resolved_underlyings: Vec<String>,
    risk: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct UniverseIntentPlan {
    profile_id: String,
    underlying: String,
    strategy_family: String,
    required_sides: String,
    min_dte: i64,
    max_dte: i64,
    selection_policy: String,
    max_expirations: usize,
}

pub(crate) async fn run() -> anyhow::Result<()> {
    let args = operator::args();
    let json_output = parse_args(&args)?;
    let config = AlpacaOptionsRuntimeConfig::from_runtime_env()?;
    let plan = build_plan(&config);
    if json_output {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        print_human_plan(&plan);
    }
    Ok(())
}

fn parse_args(args: &[String]) -> anyhow::Result<bool> {
    let mut json_output = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json_output = true,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown universe plan argument `{other}`"),
        }
    }
    Ok(json_output)
}

fn build_plan(config: &AlpacaOptionsRuntimeConfig) -> UniversePlan {
    let universe_groups = config
        .universe_groups
        .iter()
        .map(|(name, members)| UniverseGroupPlan {
            name: name.clone(),
            member_count: members.len(),
            members: members.clone(),
        })
        .collect::<Vec<_>>();
    let strategy_profiles = config
        .strategy_profiles
        .iter()
        .map(|profile| StrategyProfilePlan {
            id: profile.id.clone(),
            family: profile.family.as_str().to_string(),
            mode: profile.mode.as_str().to_string(),
            quantity: profile.quantity,
            universe_groups: profile.universe_groups.clone(),
            include_underlyings: profile.include_underlyings.clone(),
            exclude_underlyings: profile.exclude_underlyings.clone(),
            resolved_underlying_count: profile.underlyings.len(),
            resolved_underlyings: profile.underlyings.clone(),
            risk: profile.risk.to_json_value(),
        })
        .collect::<Vec<_>>();
    let universe_intents =
        option_universe_intents_from_strategy_profiles(&config.strategy_profiles)
            .into_iter()
            .map(|intent| {
                let max_expirations = intent.effective_max_expirations();
                UniverseIntentPlan {
                    profile_id: intent.profile_id,
                    underlying: intent.underlying,
                    strategy_family: intent.strategy_family.as_str().to_string(),
                    required_sides: intent.required_sides.as_str().to_string(),
                    min_dte: intent.dte_window.min_dte,
                    max_dte: intent.dte_window.max_dte,
                    selection_policy: intent.selection_policy.as_str().to_string(),
                    max_expirations,
                }
            })
            .collect::<Vec<_>>();
    let warnings = universe_plan_warnings(config);
    UniversePlan {
        checked_at_utc: Utc::now().to_rfc3339(),
        summary: UniversePlanSummary {
            universe_groups: universe_groups.len(),
            strategy_profiles: strategy_profiles.len(),
            unique_underlyings: config.underlyings.len(),
            resolved_underlyings: config.underlyings.len(),
            universe_intents: universe_intents.len(),
        },
        universe_groups,
        strategy_profiles,
        universe_intents,
        warnings,
    }
}

fn universe_plan_warnings(config: &AlpacaOptionsRuntimeConfig) -> Vec<String> {
    let mut warnings = Vec::new();
    if config.universe_groups.is_empty() {
        warnings.push("no_universe_groups_configured".to_string());
    }
    for profile in &config.strategy_profiles {
        if profile.universe_groups.is_empty() {
            warnings.push(format!(
                "profile_{}_uses_direct_underlyings_only",
                profile.id
            ));
        }
        if is_naked_family(profile.family)
            && matches!(profile.mode, AlpacaOptionsStrategyMode::Live)
        {
            warnings.push(format!("profile_{}_is_live_undefined_risk", profile.id));
        }
    }
    warnings
}

fn is_naked_family(family: AlpacaOptionsStrategyFamily) -> bool {
    matches!(
        family,
        AlpacaOptionsStrategyFamily::NakedPut
            | AlpacaOptionsStrategyFamily::NakedCall
            | AlpacaOptionsStrategyFamily::NakedPutOneToThreeDte
            | AlpacaOptionsStrategyFamily::NakedCallOneToThreeDte
    )
}

fn print_human_plan(plan: &UniversePlan) {
    println!(
        "universe_plan checked_at={} groups={} profiles={} unique_underlyings={} intents={} warnings={}",
        plan.checked_at_utc,
        plan.summary.universe_groups,
        plan.summary.strategy_profiles,
        plan.summary.unique_underlyings,
        plan.summary.universe_intents,
        plan.warnings.len()
    );
    for group in &plan.universe_groups {
        println!(
            "group {} members={} [{}]",
            group.name,
            group.member_count,
            group.members.join(",")
        );
    }
    for profile in &plan.strategy_profiles {
        println!(
            "profile {} family={} mode={} qty={} groups={} include={} exclude={} resolved={} [{}]",
            profile.id,
            profile.family,
            profile.mode,
            profile.quantity,
            profile.universe_groups.join(","),
            profile.include_underlyings.join(","),
            profile.exclude_underlyings.join(","),
            profile.resolved_underlying_count,
            profile.resolved_underlyings.join(",")
        );
    }
    for warning in &plan.warnings {
        println!("warning {warning}");
    }
}

fn print_usage() {
    println!("usage: nautilus adapters alpaca universe plan [--json]");
}
