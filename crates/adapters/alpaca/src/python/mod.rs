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

//! Python bindings for the Alpaca adapter.

use pyo3::{exceptions::PyRuntimeError, prelude::*};

use crate::{
    config::AlpacaDataClientConfig,
    http::client::AlpacaHttpClient,
    strategy::{
        PutCreditScanResult, PutCreditScannerConfig, SpreadCandidate, scan_put_credit_underlying,
    },
};

/// Python scanner configuration for put credit spreads.
#[derive(Clone, Debug)]
#[pyo3_stub_gen::derive::gen_stub_pyclass(module = "nautilus_trader.adapters.alpaca")]
#[pyclass(module = "nautilus_trader.core.nautilus_pyo3.alpaca", from_py_object)]
pub struct AlpacaPutCreditScannerConfig {
    /// Minimum days to expiration.
    #[pyo3(get, set)]
    pub min_dte: i64,
    /// Maximum days to expiration.
    #[pyo3(get, set)]
    pub max_dte: i64,
    /// Minimum absolute short-leg delta.
    #[pyo3(get, set)]
    pub short_delta_min: f64,
    /// Maximum absolute short-leg delta.
    #[pyo3(get, set)]
    pub short_delta_max: f64,
    /// Allowed spread widths.
    #[pyo3(get, set)]
    pub widths: Vec<f64>,
    /// Minimum open interest per contract.
    #[pyo3(get, set)]
    pub min_open_interest: u64,
    /// Maximum bid/ask spread as a fraction of midpoint per leg.
    #[pyo3(get, set)]
    pub max_leg_spread_pct: f64,
    /// Minimum credit / max loss.
    #[pyo3(get, set)]
    pub min_return_on_risk: f64,
    /// Minimum credit as a fraction of spread width.
    #[pyo3(get, set)]
    pub min_credit_to_width: f64,
}

impl AlpacaPutCreditScannerConfig {
    #[expect(clippy::too_many_arguments)]
    pub fn new(
        min_dte: i64,
        max_dte: i64,
        short_delta_min: f64,
        short_delta_max: f64,
        widths: Option<Vec<f64>>,
        min_open_interest: u64,
        max_leg_spread_pct: f64,
        min_return_on_risk: f64,
        min_credit_to_width: f64,
    ) -> Self {
        Self {
            min_dte,
            max_dte,
            short_delta_min,
            short_delta_max,
            widths: widths.unwrap_or_else(|| vec![2.0, 3.0, 5.0]),
            min_open_interest,
            max_leg_spread_pct,
            min_return_on_risk,
            min_credit_to_width,
        }
    }
}

#[pymethods]
#[pyo3_stub_gen::derive::gen_stub_pymethods]
impl AlpacaPutCreditScannerConfig {
    /// Creates a scanner configuration.
    #[new]
    #[pyo3(signature = (
        min_dte = 5,
        max_dte = 10,
        short_delta_min = 0.18,
        short_delta_max = 0.28,
        widths = None,
        min_open_interest = 200,
        max_leg_spread_pct = 0.15,
        min_return_on_risk = 0.13,
        min_credit_to_width = 0.08,
    ))]
    #[expect(clippy::too_many_arguments)]
    fn py_new(
        min_dte: i64,
        max_dte: i64,
        short_delta_min: f64,
        short_delta_max: f64,
        widths: Option<Vec<f64>>,
        min_open_interest: u64,
        max_leg_spread_pct: f64,
        min_return_on_risk: f64,
        min_credit_to_width: f64,
    ) -> Self {
        Self::new(
            min_dte,
            max_dte,
            short_delta_min,
            short_delta_max,
            widths,
            min_open_interest,
            max_leg_spread_pct,
            min_return_on_risk,
            min_credit_to_width,
        )
    }
}

impl From<AlpacaPutCreditScannerConfig> for PutCreditScannerConfig {
    fn from(value: AlpacaPutCreditScannerConfig) -> Self {
        Self {
            min_dte: value.min_dte,
            max_dte: value.max_dte,
            short_delta_min: value.short_delta_min,
            short_delta_max: value.short_delta_max,
            widths: value.widths,
            min_open_interest: value.min_open_interest,
            max_leg_spread_pct: value.max_leg_spread_pct,
            min_return_on_risk: value.min_return_on_risk,
            min_credit_to_width: value.min_credit_to_width,
        }
    }
}

/// Python representation of a spread candidate.
#[derive(Clone, Debug)]
#[pyo3_stub_gen::derive::gen_stub_pyclass(module = "nautilus_trader.adapters.alpaca")]
#[pyclass(
    module = "nautilus_trader.core.nautilus_pyo3.alpaca",
    skip_from_py_object
)]
pub struct AlpacaPutCreditCandidate {
    /// Short put symbol.
    #[pyo3(get)]
    pub short_symbol: String,
    /// Long put symbol.
    #[pyo3(get)]
    pub long_symbol: String,
    /// Expiration date.
    #[pyo3(get)]
    pub expiration_date: String,
    /// Spread width.
    #[pyo3(get)]
    pub width: f64,
    /// Net credit.
    #[pyo3(get)]
    pub credit: f64,
    /// Maximum loss.
    #[pyo3(get)]
    pub max_loss: f64,
    /// Return on risk.
    #[pyo3(get)]
    pub return_on_risk: f64,
    /// Short-leg absolute delta.
    #[pyo3(get)]
    pub short_delta_abs: f64,
    /// Short-leg implied volatility.
    #[pyo3(get)]
    pub implied_volatility: Option<f64>,
    /// Scanner score.
    #[pyo3(get)]
    pub score: f64,
}

impl From<SpreadCandidate> for AlpacaPutCreditCandidate {
    fn from(value: SpreadCandidate) -> Self {
        Self {
            short_symbol: value.short.symbol,
            long_symbol: value.long.symbol,
            expiration_date: value.short.expiration_date,
            width: value.width,
            credit: value.credit,
            max_loss: value.max_loss,
            return_on_risk: value.return_on_risk,
            short_delta_abs: value.short.delta_abs,
            implied_volatility: value.short.implied_volatility,
            score: value.score,
        }
    }
}

/// Python representation of a put credit scan result.
#[derive(Clone, Debug)]
#[pyo3_stub_gen::derive::gen_stub_pyclass(module = "nautilus_trader.adapters.alpaca")]
#[pyclass(
    module = "nautilus_trader.core.nautilus_pyo3.alpaca",
    skip_from_py_object
)]
pub struct AlpacaPutCreditScanResult {
    /// Underlying symbol.
    #[pyo3(get)]
    pub underlying: String,
    /// Number of contracts loaded.
    #[pyo3(get)]
    pub contract_count: usize,
    /// Number of snapshots loaded.
    #[pyo3(get)]
    pub snapshot_count: usize,
    /// Number of scoreable contracts.
    #[pyo3(get)]
    pub scoreable_count: usize,
    /// Ranked candidates.
    #[pyo3(get)]
    pub candidates: Vec<AlpacaPutCreditCandidate>,
}

impl From<PutCreditScanResult> for AlpacaPutCreditScanResult {
    fn from(value: PutCreditScanResult) -> Self {
        Self {
            underlying: value.underlying,
            contract_count: value.contract_count,
            snapshot_count: value.snapshot_count,
            scoreable_count: value.scoreable_count,
            candidates: value.candidates.into_iter().map(Into::into).collect(),
        }
    }
}

/// Runs one blocking put credit scan across the provided underlyings.
///
/// This binding is synchronous because Nautilus strategy callbacks are synchronous today.
#[pyfunction]
#[pyo3_stub_gen::derive::gen_stub_pyfunction(module = "nautilus_trader.adapters.alpaca")]
#[pyo3(name = "scan_put_credit_once", signature = (underlyings, config=None))]
pub fn scan_put_credit_once(
    py: Python<'_>,
    underlyings: Vec<String>,
    config: Option<AlpacaPutCreditScannerConfig>,
) -> PyResult<Vec<AlpacaPutCreditScanResult>> {
    py.detach(move || {
        let scanner_config = config.unwrap_or_else(|| {
            AlpacaPutCreditScannerConfig::new(5, 10, 0.18, 0.28, None, 200, 0.15, 0.13, 0.08)
        });
        let scanner_config = PutCreditScannerConfig::from(scanner_config);
        let mut data_config = AlpacaDataClientConfig::default();
        data_config.trading_base_url = std::env::var("ALPACA_TRADING_BASE_URL").ok();
        data_config.data_base_url = std::env::var("ALPACA_DATA_BASE_URL").ok();
        let client = AlpacaHttpClient::from_data_config(&data_config).map_err(to_py_runtime_err)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

        runtime.block_on(async move {
            let mut results = Vec::new();
            for underlying in underlyings {
                let result =
                    scan_put_credit_underlying(&client, &data_config, &scanner_config, underlying)
                        .await
                        .map_err(to_py_runtime_err)?;
                results.push(result.into());
            }
            Ok(results)
        })
    })
}

/// Loaded as `nautilus_pyo3.alpaca`.
#[pymodule]
pub fn alpaca(_: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<AlpacaPutCreditScannerConfig>()?;
    m.add_class::<AlpacaPutCreditCandidate>()?;
    m.add_class::<AlpacaPutCreditScanResult>()?;
    m.add_function(wrap_pyfunction!(scan_put_credit_once, m)?)?;
    Ok(())
}

fn to_py_runtime_err(error: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}
