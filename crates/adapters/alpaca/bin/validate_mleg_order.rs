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

//! Smoke utility for building and validating a non-submitting Alpaca MLeg payload.

use std::{env, process};

use nautilus_alpaca::orders::build_put_credit_spread_open_order;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() < 3 {
        eprintln!(
            "usage: alpaca-validate-mleg-order <SHORT_PUT_SYMBOL> <LONG_PUT_SYMBOL> <CREDIT_LIMIT> [QTY]"
        );
        process::exit(2);
    }

    let short_put_symbol = &args[0];
    let long_put_symbol = &args[1];
    let credit_limit = args[2].parse::<f64>()?;
    let quantity = args
        .get(3)
        .map(|value| value.parse::<u64>())
        .transpose()?
        .unwrap_or(1);

    let payload = build_put_credit_spread_open_order(
        short_put_symbol,
        long_put_symbol,
        credit_limit,
        quantity,
    )?;
    println!("{}", serde_json::to_string_pretty(&payload)?);

    Ok(())
}
