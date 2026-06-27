//! Alpaca account capability preflight for options strategy families.

use serde_json::json;

use crate::{
    http::models::{AlpacaAccount, AlpacaAccountConfiguration},
    options_runtime::AlpacaOptionsRuntimeConfig,
    runtime::emit_operator_event,
};

/// Account capability preflight result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountCapabilityPreflight {
    /// Highest options level required by the configured strategy families.
    pub required_options_level: u8,
    /// Account options-approved level from Alpaca.
    pub options_approved_level: Option<u8>,
    /// Effective account options-trading level from Alpaca.
    pub options_trading_level: Option<u8>,
    /// Account-configured maximum options trading level from Alpaca.
    pub max_options_trading_level: Option<u8>,
    /// Stable reasons blocking live submission.
    pub reasons: Vec<String>,
}

impl AccountCapabilityPreflight {
    /// Returns `true` when the account may run the configured families.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.reasons.is_empty()
    }
}

/// Evaluates Alpaca account options permissions for the configured strategy families.
#[must_use]
pub fn account_capability_preflight(
    config: &AlpacaOptionsRuntimeConfig,
    account: &AlpacaAccount,
    account_config: &AlpacaAccountConfiguration,
) -> AccountCapabilityPreflight {
    let required_options_level = required_options_level(config);
    let mut reasons = Vec::new();

    if required_options_level > 0 {
        check_level(
            "options_trading_level",
            account.options_trading_level,
            required_options_level,
            &mut reasons,
        );
        check_level(
            "options_approved_level",
            account.options_approved_level,
            required_options_level,
            &mut reasons,
        );
        check_level(
            "max_options_trading_level",
            account_config.max_options_trading_level,
            required_options_level,
            &mut reasons,
        );
        if !positive_amount(
            account
                .options_buying_power
                .as_deref()
                .or(account.buying_power.as_deref()),
        ) {
            reasons.push("options_buying_power_non_positive".to_string());
        }
    }

    if account_config.suspend_trade.unwrap_or(false) {
        reasons.push("account_configuration_suspend_trade".to_string());
    }
    if account_config.no_shorting.unwrap_or(false) && has_short_option_leg(config) {
        reasons.push("account_configuration_no_shorting".to_string());
    }

    reasons.sort();
    reasons.dedup();

    AccountCapabilityPreflight {
        required_options_level,
        options_approved_level: account.options_approved_level,
        options_trading_level: account.options_trading_level,
        max_options_trading_level: account_config.max_options_trading_level,
        reasons,
    }
}

/// Emits structured preflight evidence for operator status and logs.
pub fn emit_account_capability_preflight(
    config: &AlpacaOptionsRuntimeConfig,
    preflight: &AccountCapabilityPreflight,
) {
    emit_operator_event(
        "account_capability_preflight",
        json!({
            "result": if preflight.is_ready() { "passed" } else { "blocked" },
            "submit_enabled": config.submit_enabled,
            "strategy_families": config.enabled_strategy_family_names(),
            "required_options_level": preflight.required_options_level,
            "options_approved_level": preflight.options_approved_level,
            "options_trading_level": preflight.options_trading_level,
            "max_options_trading_level": preflight.max_options_trading_level,
            "reasons": preflight.reasons,
        }),
    );
}

fn required_options_level(config: &AlpacaOptionsRuntimeConfig) -> u8 {
    if config.enabled_strategy_family_names().is_empty() {
        0
    } else {
        // The current strategy families all submit spreads, iron condors, or short option legs.
        // Alpaca documents these as Level 3-capable strategy families.
        3
    }
}

fn has_short_option_leg(config: &AlpacaOptionsRuntimeConfig) -> bool {
    !config.enabled_strategy_family_names().is_empty()
}

fn check_level(field: &str, actual: Option<u8>, required: u8, reasons: &mut Vec<String>) {
    match actual {
        Some(actual) if actual >= required => {}
        Some(actual) => reasons.push(format!(
            "{field}_insufficient required={required} actual={actual}"
        )),
        None => reasons.push(format!("{field}_missing required={required}")),
    }
}

fn positive_amount(value: Option<&str>) -> bool {
    value
        .and_then(|value| value.trim().parse::<f64>().ok())
        .is_some_and(|value| value.is_finite() && value > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::models::{AlpacaAccount, AlpacaAccountConfiguration};

    #[test]
    fn preflight_blocks_insufficient_options_level() {
        let config = runtime_config();
        let account = account(Some(2), Some(2), Some("1000"));
        let account_config = account_config(Some(3), false, false);

        let preflight = account_capability_preflight(&config, &account, &account_config);

        assert!(!preflight.is_ready());
        assert!(
            preflight.reasons.iter().any(|reason| {
                reason == "options_trading_level_insufficient required=3 actual=2"
            })
        );
        assert!(
            preflight.reasons.iter().any(|reason| {
                reason == "options_approved_level_insufficient required=3 actual=2"
            })
        );
    }

    #[test]
    fn preflight_accepts_level_three_account() {
        let config = runtime_config();
        let account = account(Some(3), Some(3), Some("1000"));
        let account_config = account_config(Some(3), false, false);

        let preflight = account_capability_preflight(&config, &account, &account_config);

        assert!(preflight.is_ready());
    }

    #[test]
    fn preflight_blocks_account_configuration_shorting_restriction() {
        let config = runtime_config();
        let account = account(Some(3), Some(3), Some("1000"));
        let account_config = account_config(Some(3), true, false);

        let preflight = account_capability_preflight(&config, &account, &account_config);

        assert_eq!(
            preflight.reasons,
            vec!["account_configuration_no_shorting".to_string()]
        );
    }

    fn runtime_config() -> AlpacaOptionsRuntimeConfig {
        let mut config = AlpacaOptionsRuntimeConfig::from_runtime_config_for_tests();
        config.submit_enabled = true;
        config
    }

    fn account(
        options_approved_level: Option<u8>,
        options_trading_level: Option<u8>,
        options_buying_power: Option<&str>,
    ) -> AlpacaAccount {
        AlpacaAccount {
            id: None,
            account_number: None,
            status: Some("ACTIVE".to_string()),
            currency: Some("USD".to_string()),
            cash: Some("1000".to_string()),
            portfolio_value: Some("1000".to_string()),
            equity: Some("1000".to_string()),
            buying_power: Some("1000".to_string()),
            regt_buying_power: None,
            daytrading_buying_power: None,
            options_buying_power: options_buying_power.map(ToString::to_string),
            options_approved_level,
            options_trading_level,
            pattern_day_trader: None,
            trading_blocked: Some(false),
            transfers_blocked: Some(false),
            account_blocked: Some(false),
            trade_suspended_by_user: Some(false),
            multiplier: None,
        }
    }

    fn account_config(
        max_options_trading_level: Option<u8>,
        no_shorting: bool,
        suspend_trade: bool,
    ) -> AlpacaAccountConfiguration {
        AlpacaAccountConfiguration {
            dtbp_check: None,
            fractional_trading: None,
            max_margin_multiplier: None,
            max_options_trading_level,
            no_shorting: Some(no_shorting),
            pdt_check: None,
            ptp_no_exception_entry: None,
            suspend_trade: Some(suspend_trade),
            trade_confirm_email: None,
            disable_overnight_trading: None,
        }
    }
}
