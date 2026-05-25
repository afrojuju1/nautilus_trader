# -------------------------------------------------------------------------------------------------
#  Copyright (C) 2015-2026 Nautech Systems Pty Ltd. All rights reserved.
#  https://nautechsystems.io
#
#  Licensed under the GNU Lesser General Public License Version 3.0 (the "License");
#  You may not use this file except in compliance with the License.
#  You may obtain a copy of the License at https://www.gnu.org/licenses/lgpl-3.0.en.html
#
#  Unless required by applicable law or agreed to in writing, software
#  distributed under the License is distributed on an "AS IS" BASIS,
#  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
#  See the License for the specific language governing permissions and
#  limitations under the License.
# -------------------------------------------------------------------------------------------------

from decimal import Decimal

from nautilus_trader.adapters.alpaca.strategies.gap_down_fragile_rebound import (
    GapDownFragileReboundConfig,
)
from nautilus_trader.adapters.alpaca.strategies.gap_down_fragile_rebound import _BarPoint
from nautilus_trader.adapters.alpaca.strategies.gap_down_fragile_rebound import _features_for_index
from nautilus_trader.adapters.alpaca.strategies.gap_down_fragile_rebound import _passes_signal
from nautilus_trader.adapters.alpaca.strategies.gap_down_fragile_rebound import _quantile
from nautilus_trader.adapters.alpaca.strategies.gap_down_fragile_rebound import _SignalFeatures
from nautilus_trader.model.data import BarType


def test_passes_signal_accepts_fixed_bucket_fragile_rebound() -> None:
    features = _SignalFeatures(
        gap_return=-0.02,
        intraday_return=0.020408,
        close_location=0.5,
        gap_fill_pct=1.0,
        pre_drawdown_20=-0.12,
        pre_volatility_20=0.24,
        pre_rsi_14=42.0,
    )

    assert _passes_signal(features, _config())


def test_passes_signal_rejects_strong_reversal_variant() -> None:
    features = _SignalFeatures(
        gap_return=-0.02,
        intraday_return=0.020408,
        close_location=0.65,
        gap_fill_pct=0.5,
        pre_drawdown_20=-0.12,
        pre_volatility_20=0.24,
        pre_rsi_14=42.0,
    )

    assert not _passes_signal(features, _config())


def test_features_for_index_matches_fragile_gap_bucket() -> None:
    bars = _fragile_history()
    current_index = len(bars) - 1

    features = _features_for_index(
        bars,
        current_index,
        calibration_lookback_sessions=30,
        fragile_drawdown_quantile=0.4,
        high_volatility_quantile=0.67,
    )

    assert features is not None
    assert round(features.gap_return, 6) == -0.02
    assert round(features.intraday_return, 6) == 0.020408
    assert features.close_location == 0.5
    assert features.gap_fill_pct == 1.0
    assert features.pre_drawdown_20 < -0.10
    assert _passes_signal(features, _config())


def test_quantile_matches_linear_interpolation() -> None:
    assert _quantile([1.0, 2.0, 3.0, 4.0], 0.25) == 1.75


def _config() -> GapDownFragileReboundConfig:
    return GapDownFragileReboundConfig(
        bar_types=[BarType.from_str("SPY.ALPACA-1-DAY-LAST-EXTERNAL")],
        strategy_capital=Decimal(10000),
        calibration_lookback_sessions=30,
        min_history_sessions=30,
    )


def _fragile_history() -> list[_BarPoint]:
    bars: list[_BarPoint] = []
    close = 120.0

    for index in range(45):
        close *= 1.001 if index % 2 else 0.9995
        bars.append(_bar(close))

    for _ in range(20):
        close -= 1.0
        bars.append(
            _BarPoint(
                open=close + 0.2,
                high=close + 0.5,
                low=close - 0.5,
                close=close,
            ),
        )

    previous_close = bars[-1].close
    bars.append(
        _BarPoint(
            open=previous_close * 0.98,
            high=previous_close * 1.05,
            low=previous_close * 0.95,
            close=previous_close,
        ),
    )
    return bars


def _bar(close: float) -> _BarPoint:
    return _BarPoint(
        open=close * 0.995,
        high=close * 1.005,
        low=close * 0.99,
        close=close,
    )
