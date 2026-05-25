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

from nautilus_trader.adapters.alpaca.strategies.upside_gap_continuation import (
    UpsideGapContinuationConfig,
)
from nautilus_trader.adapters.alpaca.strategies.upside_gap_continuation import _BarPoint
from nautilus_trader.adapters.alpaca.strategies.upside_gap_continuation import _features_for_index
from nautilus_trader.adapters.alpaca.strategies.upside_gap_continuation import _passes_signal
from nautilus_trader.adapters.alpaca.strategies.upside_gap_continuation import _signal_score
from nautilus_trader.model.data import BarType


def test_upside_gap_continuation_accepts_risk_on_gap() -> None:
    bars = _uptrend_history()
    current_index = len(bars) - 1

    features = _features_for_index(bars, current_index)

    assert features is not None
    assert round(features.gap_return, 3) == 0.022
    assert features.close_location > 0.7
    assert features.volume_ratio_20 > 1.2
    assert features.pre_sma200_gap_pct > 0.02
    assert _passes_signal(features, _config())
    assert _signal_score(features) > features.close_location


def test_upside_gap_continuation_rejects_low_volume_gap() -> None:
    bars = _uptrend_history()
    current = bars[-1]
    bars[-1] = _BarPoint(
        open=current.open,
        high=current.high,
        low=current.low,
        close=current.close,
        volume=900.0,
    )

    features = _features_for_index(bars, len(bars) - 1)

    assert features is not None
    assert features.volume_ratio_20 < 1.2
    assert not _passes_signal(features, _config())


def test_upside_gap_continuation_requires_sufficient_history() -> None:
    assert _features_for_index(_uptrend_history()[:150], 149) is None


def _config() -> UpsideGapContinuationConfig:
    return UpsideGapContinuationConfig(
        bar_types=[BarType.from_str("SMH.ALPACA-1-DAY-LAST-EXTERNAL")],
        strategy_capital=Decimal(10000),
    )


def _uptrend_history() -> list[_BarPoint]:
    bars: list[_BarPoint] = []
    close = 80.0
    for index in range(220):
        close += 0.20 + (0.02 if index % 5 == 0 else 0.0)
        bars.append(
            _BarPoint(
                open=close - 0.10,
                high=close + 0.30,
                low=close - 0.40,
                close=close,
                volume=1000.0,
            ),
        )

    previous_close = bars[-1].close
    current_open = previous_close * 1.022
    current_close = current_open * 1.014
    bars.append(
        _BarPoint(
            open=current_open,
            high=current_close * 1.002,
            low=current_open * 0.995,
            close=current_close,
            volume=1500.0,
        ),
    )
    return bars
