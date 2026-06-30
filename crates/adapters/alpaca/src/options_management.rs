//! Nautilus-owned Alpaca options management decisions.

use chrono::{DateTime, Utc};
use nautilus_model::{data::QuoteTick, identifiers::InstrumentId};

use crate::{
    options_runtime::AlpacaOptionsRuntimeConfig,
    runtime::{StrategyStateEntry, emit_operator_event},
};
use serde_json::json;

/// Pure management thresholds for one credit-spread strategy runtime.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CreditSpreadManagementConfig {
    /// Force every active entry to close.
    pub force_flatten: bool,
    /// Close when debit is at or below this fraction of entry credit.
    pub profit_target_close_fraction: f64,
    /// Close when debit is at or above this multiple of entry credit.
    pub stop_loss_close_multiple: f64,
    /// Close after this hold time. Zero disables the trigger.
    pub max_hold_secs: u64,
    /// Close when days to expiration are at or below this value. Negative disables the trigger.
    pub expiration_exit_days: i64,
}

/// Management settings needed by the Nautilus options strategy.
#[derive(Clone, Debug, PartialEq)]
pub struct AlpacaOptionsManagementConfig {
    /// Delay between management evaluations.
    pub interval_secs: u64,
    /// Whether broker orders which close or reduce risk are enabled.
    pub close_orders_enabled: bool,
    /// Whether every active entry should be flattened.
    pub force_flatten: bool,
    /// Stale entry timeout.
    pub stale_entry_secs: u64,
    /// Stale close timeout.
    pub stale_close_secs: u64,
    /// Whether non-forced close submissions are limited to regular options hours.
    pub close_regular_hours_only: bool,
    /// Close window start.
    pub close_start: chrono::NaiveTime,
    /// Close window end.
    pub close_end: chrono::NaiveTime,
    /// Close window timezone.
    pub entry_timezone: chrono_tz::Tz,
    /// Additional debit allowed on submitted close limits.
    pub close_price_cushion: f64,
    /// Additional close cushion added per accepted close attempt.
    pub close_reprice_step: f64,
    /// Maximum total close cushion after repricing steps.
    pub max_close_price_cushion: f64,
    /// Maximum accepted close submissions per entry. Zero means unlimited.
    pub max_close_attempts: u32,
    /// Minimum delay after a close submission before another close may be submitted.
    pub close_reprice_cooldown_secs: u64,
    /// Number of high-rank candidate entries to keep quote-subscribed for active risk checks.
    pub active_risk_candidate_quote_limit: usize,
    /// Maximum accepted active-risk quote age. Zero disables freshness blocks.
    pub active_risk_quote_stale_secs: u64,
    /// Profit-target close fraction.
    pub profit_target_close_fraction: f64,
    /// Stop-loss close multiple.
    pub stop_loss_close_multiple: f64,
    /// Maximum hold time. Zero disables.
    pub max_hold_secs: u64,
    /// Expiration-risk exit days. Negative disables.
    pub expiration_exit_days: i64,
}

impl AlpacaOptionsManagementConfig {
    /// Builds management config from the Alpaca options runtime config.
    #[must_use]
    pub fn from_runtime_config(config: &AlpacaOptionsRuntimeConfig) -> Self {
        Self {
            interval_secs: config.interval_secs,
            close_orders_enabled: config.close_orders_enabled,
            force_flatten: config.force_flatten,
            stale_entry_secs: config.stale_entry_secs,
            stale_close_secs: config.stale_close_secs,
            close_regular_hours_only: config.close_regular_hours_only,
            close_start: config.close_start,
            close_end: config.close_end,
            entry_timezone: config.entry_timezone,
            close_price_cushion: config.close_price_cushion,
            close_reprice_step: config.close_reprice_step,
            max_close_price_cushion: config.max_close_price_cushion,
            max_close_attempts: config.max_close_attempts,
            close_reprice_cooldown_secs: config.close_reprice_cooldown_secs,
            active_risk_candidate_quote_limit: config.active_risk_candidate_quote_limit,
            active_risk_quote_stale_secs: config.active_risk_quote_stale_secs,
            profit_target_close_fraction: config.profit_target_close_fraction,
            stop_loss_close_multiple: config.stop_loss_close_multiple,
            max_hold_secs: config.max_hold_secs,
            expiration_exit_days: config.expiration_exit_days,
        }
    }

    /// Returns `true` if non-forced close submissions are currently allowed.
    #[must_use]
    pub fn inside_close_window(&self, now: DateTime<Utc>) -> bool {
        self.force_flatten || !self.close_regular_hours_only || {
            let now = now.with_timezone(&self.entry_timezone).time();
            self.close_start <= now && now <= self.close_end
        }
    }
}

impl Default for AlpacaOptionsManagementConfig {
    fn default() -> Self {
        Self {
            interval_secs: 60,
            close_orders_enabled: false,
            force_flatten: false,
            stale_entry_secs: 900,
            stale_close_secs: 900,
            close_regular_hours_only: true,
            close_start: chrono::NaiveTime::from_hms_opt(9, 30, 0).expect("valid time"),
            close_end: chrono::NaiveTime::from_hms_opt(16, 0, 0).expect("valid time"),
            entry_timezone: chrono_tz::America::New_York,
            close_price_cushion: 0.0,
            close_reprice_step: 0.0,
            max_close_price_cushion: 0.0,
            max_close_attempts: 0,
            close_reprice_cooldown_secs: 0,
            active_risk_candidate_quote_limit: 5,
            active_risk_quote_stale_secs: 30,
            profit_target_close_fraction: 0.50,
            stop_loss_close_multiple: 2.0,
            max_hold_secs: 0,
            expiration_exit_days: -1,
        }
    }
}

/// Quote snapshot used to price a close order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloseQuote {
    /// Ask on the short put/primary short leg.
    pub short_ask: f64,
    /// Bid on the long put/primary long leg.
    pub long_bid: f64,
    /// Ask on the short call leg for iron condors.
    pub short_call_ask: Option<f64>,
    /// Bid on the long call leg for iron condors.
    pub long_call_bid: Option<f64>,
    /// Net close debit. Debit spreads store close credit as a negative debit.
    pub debit: f64,
}

impl CloseQuote {
    /// Applies an additional close-price cushion to short-leg ask prices.
    #[must_use]
    pub fn with_price_cushion(self, cushion: f64) -> Self {
        let cushion = cushion.max(0.0);
        if cushion == 0.0 {
            return self;
        }

        let mut quote = self;
        let short_leg_count = if quote.short_call_ask.is_some() {
            2.0
        } else {
            1.0
        };
        let per_short_leg_cushion = cushion / short_leg_count;
        quote.short_ask += per_short_leg_cushion;
        if let Some(short_call_ask) = quote.short_call_ask.as_mut() {
            *short_call_ask += per_short_leg_cushion;
        }
        quote.debit += cushion;
        quote
    }
}

/// Builds a close quote from Nautilus quote ticks already owned by the runtime cache.
#[must_use]
pub fn close_quote_from_ticks(
    entry: &StrategyStateEntry,
    short_quote: Option<QuoteTick>,
    long_quote: Option<QuoteTick>,
    short_call_quote: Option<QuoteTick>,
    long_call_quote: Option<QuoteTick>,
) -> Option<CloseQuote> {
    let short_ask = positive_price(short_quote?.ask_price.as_f64())?;
    if entry.is_naked_option() {
        return Some(CloseQuote {
            short_ask,
            long_bid: 0.0,
            short_call_ask: None,
            long_call_bid: None,
            debit: short_ask,
        });
    }

    let long_bid = positive_price(long_quote?.bid_price.as_f64())?;
    let mut debit = if entry.is_debit_spread() {
        let credit = long_bid - short_ask;
        if credit <= 0.0 {
            return None;
        }
        -credit
    } else {
        short_ask - long_bid
    };

    let mut short_call_ask = None;
    let mut long_call_bid = None;
    if entry.short_call_symbol.is_some() || entry.long_call_symbol.is_some() {
        let call_short_ask = positive_price(short_call_quote?.ask_price.as_f64())?;
        let call_long_bid = positive_price(long_call_quote?.bid_price.as_f64())?;
        debit += call_short_ask - call_long_bid;
        short_call_ask = Some(call_short_ask);
        long_call_bid = Some(call_long_bid);
    }

    if !entry.is_debit_spread() && debit <= 0.0 {
        return None;
    }

    Some(CloseQuote {
        short_ask,
        long_bid,
        short_call_ask,
        long_call_bid,
        debit,
    })
}

/// Returns the close reason for one active entry.
#[must_use]
pub fn close_reason(
    config: &AlpacaOptionsManagementConfig,
    entry: &StrategyStateEntry,
    close_debit: f64,
) -> Option<String> {
    if entry.is_debit_spread() {
        debit_spread_close_reason(config, entry, -close_debit)
    } else {
        credit_spread_close_reason(&credit_management_config(config), entry, close_debit)
            .map(ToString::to_string)
    }
}

/// Evaluates whether a credit spread should be closed at the current debit.
#[must_use]
pub fn credit_spread_close_reason(
    config: &CreditSpreadManagementConfig,
    entry: &StrategyStateEntry,
    close_debit: f64,
) -> Option<&'static str> {
    if config.force_flatten {
        return Some("manual_flatten");
    }
    if close_debit <= entry.credit * config.profit_target_close_fraction {
        return Some("profit_target");
    }
    if close_debit >= entry.credit * config.stop_loss_close_multiple {
        return Some("stop_loss");
    }
    if config.max_hold_secs > 0
        && recorded_age_secs(entry).is_some_and(|age| age >= config.max_hold_secs)
    {
        return Some("max_hold");
    }
    if config.expiration_exit_days >= 0
        && days_to_expiration(&entry.short_symbol)
            .is_some_and(|days| days <= config.expiration_exit_days)
    {
        return Some("expiration_risk");
    }
    None
}

/// Returns the age in seconds from the state entry's recorded timestamp.
#[must_use]
pub fn recorded_age_secs(entry: &StrategyStateEntry) -> Option<u64> {
    age_secs_from_rfc3339(&entry.recorded_at_utc)
}

/// Parses an Alpaca option symbol and returns calendar days to expiration.
#[must_use]
pub fn days_to_expiration(symbol: &str) -> Option<i64> {
    let chars = symbol.as_bytes();
    for index in 0..chars.len().saturating_sub(6) {
        let date_slice = &chars[index..index + 6];
        let put_call = chars.get(index + 6).copied();
        if date_slice.iter().all(u8::is_ascii_digit) && matches!(put_call, Some(b'P' | b'C')) {
            let value = std::str::from_utf8(date_slice).ok()?;
            let year = 2000 + value[0..2].parse::<i32>().ok()?;
            let month = value[2..4].parse::<u32>().ok()?;
            let day = value[4..6].parse::<u32>().ok()?;
            let expiration = chrono::NaiveDate::from_ymd_opt(year, month, day)?;
            return Some(
                expiration
                    .signed_duration_since(Utc::now().date_naive())
                    .num_days(),
            );
        }
    }
    None
}

/// Returns `true` when close attempts are exhausted.
#[must_use]
pub fn close_attempts_exhausted(
    config: &AlpacaOptionsManagementConfig,
    entry: &StrategyStateEntry,
) -> bool {
    !config.force_flatten
        && config.max_close_attempts > 0
        && entry.close_attempts >= config.max_close_attempts
}

/// Returns reprice cooldown seconds remaining, if a close replacement is blocked.
#[must_use]
pub fn close_reprice_cooldown_remaining_secs(
    config: &AlpacaOptionsManagementConfig,
    entry: &StrategyStateEntry,
) -> Option<u64> {
    if config.force_flatten || config.close_reprice_cooldown_secs == 0 {
        return None;
    }

    let age = entry
        .last_close_submitted_at_utc
        .as_deref()
        .and_then(age_secs_from_rfc3339)?;
    (age < config.close_reprice_cooldown_secs).then_some(config.close_reprice_cooldown_secs - age)
}

/// Returns the bounded close price cushion for the next close submission attempt.
#[must_use]
pub fn close_price_cushion_for_attempt(
    config: &AlpacaOptionsManagementConfig,
    entry: &StrategyStateEntry,
) -> f64 {
    let cushion =
        config.close_price_cushion + config.close_reprice_step * entry.close_attempts as f64;
    cushion.max(0.0).min(
        config
            .max_close_price_cushion
            .max(config.close_price_cushion),
    )
}

/// Emits a management snapshot operator event.
pub fn emit_management_snapshot(
    entry: &StrategyStateEntry,
    close_quote: &CloseQuote,
    close_reason: Option<&str>,
) {
    let (net_premium_kind, entry_net_premium, close_net_premium, unrealized_pnl) =
        if entry.is_debit_spread() {
            let close_credit = -close_quote.debit;
            (
                "debit",
                entry.entry_debit(),
                Some(close_credit),
                entry.entry_debit().map(|debit| close_credit - debit),
            )
        } else {
            (
                "credit",
                Some(entry.credit),
                Some(close_quote.debit),
                Some(entry.credit - close_quote.debit),
            )
        };
    let unrealized_pnl_fraction = unrealized_pnl.and_then(|pnl| {
        entry_net_premium.and_then(|basis| if basis > 0.0 { Some(pnl / basis) } else { None })
    });
    emit_operator_event(
        "management_snapshot",
        json!({
            "underlying": entry.underlying,
            "strategy": entry.strategy,
            "net_premium_kind": net_premium_kind,
            "entry_net_premium": entry_net_premium,
            "close_net_premium": close_net_premium,
            "unrealized_pnl": unrealized_pnl,
            "unrealized_pnl_fraction": unrealized_pnl_fraction,
            "close_reason": close_reason,
            "close_attempts": entry.close_attempts,
            "days_to_expiration": days_to_expiration(&entry.short_symbol)
                .or_else(|| days_to_expiration(&entry.long_symbol)),
            "hold_secs": recorded_age_secs(entry),
        }),
    );
}

/// Returns all Alpaca option instrument IDs needed for management quotes.
pub fn management_instrument_ids(entry: &StrategyStateEntry) -> anyhow::Result<Vec<InstrumentId>> {
    let mut symbols = entry.symbols();
    symbols.sort_unstable();
    symbols.dedup();
    symbols
        .into_iter()
        .map(alpaca_instrument_id)
        .collect::<anyhow::Result<Vec<_>>>()
}

/// Builds an Alpaca option instrument ID from a symbol.
pub fn alpaca_instrument_id(symbol: &str) -> anyhow::Result<InstrumentId> {
    Ok(format!("{symbol}.ALPACA").parse()?)
}

fn positive_price(value: f64) -> Option<f64> {
    (value > 0.0).then_some(value)
}

fn credit_management_config(
    config: &AlpacaOptionsManagementConfig,
) -> CreditSpreadManagementConfig {
    CreditSpreadManagementConfig {
        force_flatten: config.force_flatten,
        profit_target_close_fraction: config.profit_target_close_fraction,
        stop_loss_close_multiple: config.stop_loss_close_multiple,
        max_hold_secs: config.max_hold_secs,
        expiration_exit_days: config.expiration_exit_days,
    }
}

fn debit_spread_close_reason(
    config: &AlpacaOptionsManagementConfig,
    entry: &StrategyStateEntry,
    close_credit: f64,
) -> Option<String> {
    if config.force_flatten {
        return Some("manual_flatten".to_string());
    }
    let entry_debit = entry.entry_debit()?;
    if close_credit >= entry_debit * (1.0 + config.profit_target_close_fraction.max(0.0)) {
        return Some("profit_target".to_string());
    }
    if config.stop_loss_close_multiple > 0.0
        && close_credit <= entry_debit / config.stop_loss_close_multiple
    {
        return Some("stop_loss".to_string());
    }
    if config.max_hold_secs > 0
        && DateTime::parse_from_rfc3339(&entry.recorded_at_utc)
            .map(|recorded| {
                Utc::now()
                    .signed_duration_since(recorded.with_timezone(&Utc))
                    .num_seconds()
                    >= config.max_hold_secs as i64
            })
            .unwrap_or(false)
    {
        return Some("max_hold".to_string());
    }
    if config.expiration_exit_days >= 0
        && days_to_expiration(&entry.short_symbol)
            .or_else(|| days_to_expiration(&entry.long_symbol))
            .is_some_and(|days| days <= config.expiration_exit_days)
    {
        return Some("expiration_risk".to_string());
    }
    None
}

fn age_secs_from_rfc3339(value: &str) -> Option<u64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|timestamp| {
            Utc::now()
                .signed_duration_since(timestamp.with_timezone(&Utc))
                .to_std()
                .ok()
        })
        .map(|duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_price_cushion_ladder_is_attempt_based_and_capped() {
        let config = AlpacaOptionsManagementConfig {
            close_price_cushion: 0.02,
            close_reprice_step: 0.01,
            max_close_price_cushion: 0.05,
            ..AlpacaOptionsManagementConfig::default()
        };
        let mut entry = state_entry();

        assert_eq!(close_price_cushion_for_attempt(&config, &entry), 0.02);

        entry.close_attempts = 2;
        assert_eq!(close_price_cushion_for_attempt(&config, &entry), 0.04);

        entry.close_attempts = 10;
        assert_eq!(close_price_cushion_for_attempt(&config, &entry), 0.05);
    }

    fn state_entry() -> StrategyStateEntry {
        StrategyStateEntry {
            trade_date: "2026-05-07".to_string(),
            underlying: "SPY".to_string(),
            strategy: "call_credit".to_string(),
            order_list_id: "entry-list".to_string(),
            short_symbol: "SPY260515C00720000".to_string(),
            long_symbol: "SPY260515C00722000".to_string(),
            short_call_symbol: None,
            long_call_symbol: None,
            quantity: 1,
            credit: 0.40,
            debit: None,
            risk_capital_usd: Some(160.0),
            score: 72.5,
            parent_order_id: Some("open-parent".to_string()),
            submitted_at_utc: Some("2026-05-07T14:00:00Z".to_string()),
            close_order_list_id: Some("close-list".to_string()),
            close_parent_order_id: Some("close-parent".to_string()),
            close_reason: Some("stop_loss".to_string()),
            close_attempts: 0,
            last_close_submitted_at_utc: None,
            submitted: true,
            canceled: false,
            closed: false,
            recorded_at_utc: "2026-05-07T14:00:00Z".to_string(),
            closed_at_utc: None,
        }
    }
}
