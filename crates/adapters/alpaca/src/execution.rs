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

//! Execution admission helpers for Alpaca option-spread strategies.

use std::collections::BTreeSet;

use crate::http::models::{AlpacaAccount, AlpacaOrder, AlpacaPosition};

/// Admission result for a candidate option spread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionDecision {
    /// If the candidate can be submitted.
    pub allowed: bool,
    /// Human-readable rejection reasons.
    pub reasons: Vec<String>,
}

impl AdmissionDecision {
    /// Returns an allowed admission result.
    #[must_use]
    pub fn allow() -> Self {
        Self {
            allowed: true,
            reasons: Vec::new(),
        }
    }

    /// Returns a rejected admission result.
    #[must_use]
    pub fn reject(reasons: Vec<String>) -> Self {
        Self {
            allowed: false,
            reasons,
        }
    }
}

/// Checks account, position, and open-order state before opening a put credit spread.
#[must_use]
pub fn check_put_credit_entry_admission(
    account: &AlpacaAccount,
    positions: &[AlpacaPosition],
    open_orders: &[AlpacaOrder],
    short_put_symbol: &str,
    long_put_symbol: &str,
) -> AdmissionDecision {
    let mut reasons = Vec::new();
    check_account(account, &mut reasons);

    let candidate_underlying = option_underlying_symbol(short_put_symbol);
    if candidate_underlying.is_empty()
        || candidate_underlying != option_underlying_symbol(long_put_symbol)
    {
        reasons.push("candidate legs must resolve to the same option underlying".to_string());
    }

    let candidate_symbols = BTreeSet::from([
        short_put_symbol.trim().to_string(),
        long_put_symbol.trim().to_string(),
    ]);

    for position in positions {
        let Some(symbol) = position.symbol.as_deref() else {
            continue;
        };
        if !has_nonzero_quantity(position.qty.as_deref()) {
            continue;
        }
        let position_underlying = option_underlying_symbol(symbol);
        if candidate_symbols.contains(symbol) {
            reasons.push(format!("existing open position on candidate leg {symbol}"));
        } else if !candidate_underlying.is_empty() && position_underlying == candidate_underlying {
            reasons.push(format!(
                "existing open option position on underlying {candidate_underlying}: {symbol}",
            ));
        }
    }

    for order in open_orders.iter().filter(|order| order.is_working()) {
        for symbol in order.symbols() {
            let order_underlying = option_underlying_symbol(&symbol);
            if candidate_symbols.contains(&symbol) {
                reasons.push(format!(
                    "working order already references candidate leg {symbol}"
                ));
            } else if !candidate_underlying.is_empty() && order_underlying == candidate_underlying {
                reasons.push(format!(
                    "working order already references underlying {candidate_underlying}: {symbol}",
                ));
            }
        }
    }

    if reasons.is_empty() {
        AdmissionDecision::allow()
    } else {
        reasons.sort();
        reasons.dedup();
        AdmissionDecision::reject(reasons)
    }
}

/// Extracts the OCC-style underlying root from an Alpaca option contract symbol.
#[must_use]
pub fn option_underlying_symbol(symbol: &str) -> String {
    let mut root = String::new();
    for character in symbol.trim().chars() {
        if character.is_ascii_digit() {
            break;
        }
        root.push(character);
    }
    root
}

fn check_account(account: &AlpacaAccount, reasons: &mut Vec<String>) {
    if account.status.as_deref() != Some("ACTIVE") {
        reasons.push(format!(
            "account status is {}",
            account.status.as_deref().unwrap_or("unknown"),
        ));
    }
    if account.trading_blocked.unwrap_or(false) {
        reasons.push("account trading_blocked is true".to_string());
    }
    if account.account_blocked.unwrap_or(false) {
        reasons.push("account_blocked is true".to_string());
    }
    if account.trade_suspended_by_user.unwrap_or(false) {
        reasons.push("trade_suspended_by_user is true".to_string());
    }
}

fn has_nonzero_quantity(quantity: Option<&str>) -> bool {
    quantity
        .and_then(|value| value.parse::<f64>().ok())
        .is_some_and(|value| value != 0.0)
}
