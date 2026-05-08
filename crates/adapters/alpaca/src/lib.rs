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

//! [NautilusTrader](https://nautilustrader.io) adapter scaffold for
//! [Alpaca Markets](https://alpaca.markets/).
//!
//! The `nautilus-alpaca` crate currently defines shared configuration and endpoint constants for
//! the planned live market data and execution adapter. The first production target is US equity and
//! US equity option workflows, including short-dated multi-leg option spreads submitted through
//! Alpaca paper trading.
//!
//! # Feature flags
//!
//! - `python`: Enables Python bindings once the Rust clients are exposed through PyO3.
//! - `extension-module`: Builds as a Python extension module (used together with `python`).
//! - `high-precision`: Reserved for parity with the Nautilus adapter workspace.

#![warn(rustc::all)]
#![deny(unsafe_code)]
#![deny(nonstandard_style)]
#![deny(missing_debug_implementations)]
#![deny(clippy::missing_errors_doc)]
#![deny(clippy::missing_panics_doc)]
#![deny(rustdoc::broken_intra_doc_links)]

#[cfg(feature = "live")]
pub mod candidate_ledger;
pub mod common;
pub mod config;
pub mod earnings;
pub mod execution;
#[cfg(feature = "live")]
pub mod factories;
#[cfg(feature = "live")]
pub mod fleet;
pub mod http;
#[cfg(feature = "live")]
pub mod management;
#[cfg(feature = "live")]
pub mod options_engine;
#[cfg(feature = "live")]
pub mod options_runtime;
pub mod orders;
pub mod parse;
#[cfg(feature = "live")]
pub mod performance;
pub mod providers;
#[cfg(feature = "python")]
pub mod python;
#[cfg(feature = "live")]
pub mod runtime;
#[cfg(feature = "live")]
pub mod runtime_env;
pub mod strategy;
#[cfg(feature = "live")]
pub mod submit;
#[cfg(feature = "live")]
pub mod websocket;

#[cfg(feature = "live")]
pub use execution::AlpacaExecutionClient;
#[cfg(feature = "live")]
pub use factories::AlpacaExecutionClientFactory;
