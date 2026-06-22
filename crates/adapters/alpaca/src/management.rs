// -------------------------------------------------------------------------------------------------
//  Copyright (C) 2015-2026 Nautech Systems Pty Ltd. All rights reserved.
//  https://nautechsystems.io
//
//  Licensed under the GNU Lesser General Public License Version 3.0 (the "License");
//  You may not use this file except in compliance with the License.
//  You may obtain a copy of the License at https://www.gnu.org/licenses/lgpl-3.0.en.html
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
// -------------------------------------------------------------------------------------------------

//! Credit-spread management decisions shared by Alpaca runtime binaries.

use chrono::{DateTime, Utc};

use crate::runtime::StrategyStateEntry;

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
    use crate::{
        candidate_engine::CreditSpreadKind,
        management::{CreditSpreadManagementConfig, credit_spread_close_reason},
        runtime::{StrategyStateEntry, credit_spread_strategy_name},
    };

    #[test]
    fn close_reason_prefers_manual_flatten() {
        let config = CreditSpreadManagementConfig {
            force_flatten: true,
            ..default_config()
        };

        assert_eq!(
            credit_spread_close_reason(&config, &entry(), 99.0),
            Some("manual_flatten"),
        );
    }

    #[test]
    fn close_reason_detects_profit_target_and_stop_loss() {
        assert_eq!(
            credit_spread_close_reason(&default_config(), &entry(), 0.20),
            Some("profit_target"),
        );
        assert_eq!(
            credit_spread_close_reason(&default_config(), &entry(), 1.00),
            Some("stop_loss"),
        );
    }

    #[test]
    fn close_reason_detects_max_hold() {
        let config = CreditSpreadManagementConfig {
            max_hold_secs: 1,
            profit_target_close_fraction: 0.0,
            stop_loss_close_multiple: 999.0,
            ..default_config()
        };

        assert_eq!(
            credit_spread_close_reason(&config, &entry(), 0.60),
            Some("max_hold"),
        );
    }

    fn default_config() -> CreditSpreadManagementConfig {
        CreditSpreadManagementConfig {
            force_flatten: false,
            profit_target_close_fraction: 0.50,
            stop_loss_close_multiple: 2.0,
            max_hold_secs: 0,
            expiration_exit_days: -1,
        }
    }

    fn entry() -> StrategyStateEntry {
        StrategyStateEntry {
            trade_date: "2026-05-02".to_string(),
            underlying: "SPY".to_string(),
            strategy: credit_spread_strategy_name(CreditSpreadKind::Put).to_string(),
            order_list_id: "order-list-1".to_string(),
            short_symbol: "SPY260512P00708000".to_string(),
            long_symbol: "SPY260512P00705000".to_string(),
            short_call_symbol: None,
            long_call_symbol: None,
            quantity: 1,
            credit: 0.50,
            debit: None,
            score: 60.0,
            parent_order_id: Some("parent-1".to_string()),
            close_order_list_id: None,
            close_parent_order_id: None,
            close_reason: None,
            close_attempts: 0,
            last_close_submitted_at_utc: None,
            submitted: true,
            canceled: false,
            closed: false,
            recorded_at_utc: "1970-01-01T00:00:00Z".to_string(),
            closed_at_utc: None,
        }
    }
}
