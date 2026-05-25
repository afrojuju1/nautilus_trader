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
"""
Daily-bar ETF upside-gap continuation strategy migrated from ``upside_gap_continuation_v1``.
"""

from __future__ import annotations

from collections import deque
from dataclasses import dataclass
from decimal import ROUND_FLOOR
from decimal import Decimal
from math import sqrt
from statistics import stdev

import pandas as pd

from nautilus_trader.common.config import PositiveInt
from nautilus_trader.config import StrategyConfig
from nautilus_trader.model.data import Bar
from nautilus_trader.model.data import BarType
from nautilus_trader.model.enums import OrderSide
from nautilus_trader.model.enums import TimeInForce
from nautilus_trader.model.identifiers import InstrumentId
from nautilus_trader.model.instruments import Instrument
from nautilus_trader.model.orders import LimitOrder
from nautilus_trader.trading.strategy import Strategy


STRATEGY_TAG = "upside_gap_continuation_v1"


class UpsideGapContinuationConfig(StrategyConfig, frozen=True):
    """
    Configuration for ``UpsideGapContinuation``.
    """

    bar_types: list[BarType]
    strategy_capital: Decimal
    capital_fraction: float = 1.0
    request_historical_bars: bool = True
    history_days: PositiveInt = 420
    minimum_history_sessions: PositiveInt = 200
    hold_bars: PositiveInt = 5
    limit_offset_bps: float = 5.0
    entry_fill_timeout_bars: PositiveInt = 2
    gap_return_gte: float = 0.02
    pre_sma200_gap_pct_gte: float = 0.02
    pre_sma50_gap_pct_gte: float = 0.01
    pre_momentum_5_gte: float = 0.0
    intraday_return_gte: float = -0.005
    close_location_gte: float = 0.7
    volume_ratio_20_gte: float = 1.2
    pre_volatility_20_lte: float = 0.327635


@dataclass(frozen=True)
class _BarPoint:
    open: float
    high: float
    low: float
    close: float
    volume: float


@dataclass(frozen=True)
class _SignalFeatures:
    gap_return: float
    intraday_return: float
    close_location: float
    volume_ratio_20: float
    pre_sma200_gap_pct: float
    pre_sma50_gap_pct: float
    pre_momentum_5: float
    pre_volatility_20: float


@dataclass(frozen=True)
class _SignalCandidate:
    bar_type: BarType
    instrument_id: InstrumentId
    close: float
    score: float
    features: _SignalFeatures


@dataclass
class _ActiveTrade:
    bar_type: BarType
    instrument_id: InstrumentId
    quantity: Decimal
    bars_held: int = 0
    entry_wait_bars: int = 0
    exit_submitted: bool = False


class UpsideGapContinuation(Strategy):
    """
    Regular Nautilus strategy for the risk-on ETF upside-gap continuation book.
    """

    def __init__(self, config: UpsideGapContinuationConfig) -> None:
        if not config.bar_types:
            raise ValueError("bar_types must not be empty")
        if config.strategy_capital <= Decimal(0):
            raise ValueError("strategy_capital must be positive")
        if not 0.0 < config.capital_fraction <= 1.0:
            raise ValueError("capital_fraction must be in the range (0, 1]")
        if config.limit_offset_bps < 0.0:
            raise ValueError("limit_offset_bps must be non-negative")
        super().__init__(config)

        max_history = config.minimum_history_sessions + config.hold_bars + 40
        self._bar_types = tuple(config.bar_types)
        self._bar_type_set = set(config.bar_types)
        self._histories: dict[BarType, deque[_BarPoint]] = {
            bar_type: deque(maxlen=max_history) for bar_type in self._bar_types
        }
        self._instruments: dict[InstrumentId, Instrument] = {}
        self._pending_candidates: dict[int, list[_SignalCandidate]] = {}
        self._seen_bar_types: dict[int, set[BarType]] = {}
        self._finalized_sessions: set[int] = set()
        self._active_trade: _ActiveTrade | None = None

    def on_start(self) -> None:
        """
        Load instruments and subscribe to configured daily bars.
        """
        for bar_type in self._bar_types:
            instrument_id = bar_type.instrument_id
            instrument = self.cache.instrument(instrument_id)
            if instrument is None:
                self.log.error(f"Could not find instrument for {instrument_id}")
                self.stop()
                return
            self._instruments[instrument_id] = instrument

            if self.config.request_historical_bars:
                self.request_bars(
                    bar_type,
                    start=self._clock.utc_now() - pd.Timedelta(days=self.config.history_days),
                )
            self.subscribe_bars(bar_type)

    def on_bar(self, bar: Bar) -> None:
        """
        Process one completed daily bar.
        """
        bar_type = bar.bar_type
        if bar_type not in self._bar_type_set:
            return
        if bar.is_revision:
            return

        session_key = int(bar.ts_event)
        self._finalize_stale_sessions(session_key)
        self._manage_active_trade(bar_type, bar)

        history = self._histories[bar_type]
        candidate = self._candidate_from_bar(bar_type, bar, history)
        history.append(_bar_point(bar))

        self._seen_bar_types.setdefault(session_key, set()).add(bar_type)
        if candidate is not None:
            self._pending_candidates.setdefault(session_key, []).append(candidate)

        if len(self._seen_bar_types[session_key]) == len(self._bar_types):
            self._finalize_session(session_key)

    def on_stop(self) -> None:
        """
        Cancel outstanding orders and unsubscribe bars.
        """
        for bar_type in self._bar_types:
            self.cancel_all_orders(bar_type.instrument_id)
            self.unsubscribe_bars(bar_type)

    def on_reset(self) -> None:
        """
        Reset strategy-local state.
        """
        for history in self._histories.values():
            history.clear()
        self._pending_candidates.clear()
        self._seen_bar_types.clear()
        self._finalized_sessions.clear()
        self._active_trade = None

    def _candidate_from_bar(
        self,
        bar_type: BarType,
        bar: Bar,
        history: deque[_BarPoint],
    ) -> _SignalCandidate | None:
        if len(history) < self.config.minimum_history_sessions:
            return None
        if bar.is_single_price():
            return None

        point = _bar_point(bar)
        features = _features_for_index([*history, point], len(history))
        if features is None or not _passes_signal(features, self.config):
            return None

        return _SignalCandidate(
            bar_type=bar_type,
            instrument_id=bar_type.instrument_id,
            close=point.close,
            score=_signal_score(features),
            features=features,
        )

    def _finalize_stale_sessions(self, current_session_key: int) -> None:
        for session_key in sorted(self._seen_bar_types):
            if session_key < current_session_key:
                self._finalize_session(session_key)

    def _finalize_session(self, session_key: int) -> None:
        if session_key in self._finalized_sessions:
            return
        self._finalized_sessions.add(session_key)
        self._seen_bar_types.pop(session_key, None)
        candidates = self._pending_candidates.pop(session_key, [])
        if not candidates:
            return
        if not self._can_open_new_position():
            self.log.debug(f"Skipping {STRATEGY_TAG} signal while position/order is active")
            return

        best = max(
            candidates,
            key=lambda candidate: (
                candidate.score,
                candidate.features.volume_ratio_20,
                candidate.features.gap_return,
            ),
        )
        self._submit_entry(best)

    def _can_open_new_position(self) -> bool:
        if self._active_trade is not None:
            return False
        return all(self.portfolio.is_flat(bar_type.instrument_id) for bar_type in self._bar_types)

    def _submit_entry(self, candidate: _SignalCandidate) -> None:
        instrument = self._instruments[candidate.instrument_id]
        quantity_decimal = self._quantity_for_price(candidate.close)
        if quantity_decimal <= Decimal(0):
            self.log.warning(
                f"Skipping {STRATEGY_TAG} entry for {candidate.instrument_id}: quantity is zero",
            )
            return

        quantity = instrument.make_qty(quantity_decimal)
        limit_price = candidate.close * (1.0 + self.config.limit_offset_bps / 10_000.0)
        order: LimitOrder = self.order_factory.limit(
            instrument_id=candidate.instrument_id,
            order_side=OrderSide.BUY,
            quantity=quantity,
            price=instrument.make_price(limit_price),
            time_in_force=TimeInForce.DAY,
            tags=[STRATEGY_TAG],
        )
        self.submit_order(order)
        self._active_trade = _ActiveTrade(
            bar_type=candidate.bar_type,
            instrument_id=candidate.instrument_id,
            quantity=quantity_decimal,
        )
        self.log.info(
            f"{STRATEGY_TAG} entry submitted {candidate.instrument_id} "
            f"qty={quantity_decimal} score={candidate.score:.6f}",
        )

    def _manage_active_trade(self, bar_type: BarType, bar: Bar) -> None:
        active = self._active_trade
        if active is None or active.bar_type != bar_type:
            return

        if active.exit_submitted:
            if self.portfolio.is_flat(active.instrument_id):
                self._active_trade = None
            return

        if not self.portfolio.is_net_long(active.instrument_id):
            active.entry_wait_bars += 1
            if active.entry_wait_bars >= self.config.entry_fill_timeout_bars:
                self.cancel_all_orders(active.instrument_id)
                self._active_trade = None
                self.log.warning(
                    f"Canceled stale {STRATEGY_TAG} entry for {active.instrument_id}",
                )
            return

        active.entry_wait_bars = 0
        active.bars_held += 1
        if active.bars_held >= self.config.hold_bars:
            self._submit_exit(active, bar.close.as_double())

    def _submit_exit(self, active: _ActiveTrade, close: float) -> None:
        instrument = self._instruments[active.instrument_id]
        position_qty = self.portfolio.net_position(active.instrument_id)
        quantity_decimal = min(abs(position_qty), active.quantity)
        if quantity_decimal <= Decimal(0):
            self._active_trade = None
            return

        limit_price = close * (1.0 - self.config.limit_offset_bps / 10_000.0)
        order: LimitOrder = self.order_factory.limit(
            instrument_id=active.instrument_id,
            order_side=OrderSide.SELL,
            quantity=instrument.make_qty(quantity_decimal),
            price=instrument.make_price(limit_price),
            time_in_force=TimeInForce.DAY,
            reduce_only=True,
            tags=[STRATEGY_TAG],
        )
        self.submit_order(order)
        active.exit_submitted = True
        self.log.info(
            f"{STRATEGY_TAG} exit submitted {active.instrument_id} qty={quantity_decimal}",
        )

    def _quantity_for_price(self, price: float) -> Decimal:
        capital = self.config.strategy_capital * Decimal(str(self.config.capital_fraction))
        return (capital / Decimal(str(price))).to_integral_value(rounding=ROUND_FLOOR)


def _bar_point(bar: Bar) -> _BarPoint:
    return _BarPoint(
        open=bar.open.as_double(),
        high=bar.high.as_double(),
        low=bar.low.as_double(),
        close=bar.close.as_double(),
        volume=bar.volume.as_double(),
    )


def _features_for_index(bars: list[_BarPoint], index: int) -> _SignalFeatures | None:
    if index < 200:
        return None

    current = bars[index]
    previous = bars[index - 1]
    if previous.close <= 0.0 or current.open <= 0.0:
        return None

    high_low_range = current.high - current.low
    if high_low_range <= 0.0:
        return None

    sma200 = _mean_close(bars[index - 200 : index])
    sma50 = _mean_close(bars[index - 50 : index])
    volatility = _pre_volatility_20(bars, index)
    average_volume_20 = _mean_volume(bars[index - 20 : index])
    if sma200 <= 0.0 or sma50 <= 0.0 or volatility is None or average_volume_20 <= 0.0:
        return None

    return _SignalFeatures(
        gap_return=current.open / previous.close - 1.0,
        intraday_return=current.close / current.open - 1.0,
        close_location=(current.close - current.low) / high_low_range,
        volume_ratio_20=current.volume / average_volume_20,
        pre_sma200_gap_pct=previous.close / sma200 - 1.0,
        pre_sma50_gap_pct=previous.close / sma50 - 1.0,
        pre_momentum_5=previous.close / bars[index - 6].close - 1.0,
        pre_volatility_20=volatility,
    )


def _passes_signal(features: _SignalFeatures, config: UpsideGapContinuationConfig) -> bool:
    return (
        features.gap_return >= config.gap_return_gte
        and features.pre_sma200_gap_pct >= config.pre_sma200_gap_pct_gte
        and features.pre_sma50_gap_pct >= config.pre_sma50_gap_pct_gte
        and features.pre_momentum_5 >= config.pre_momentum_5_gte
        and features.intraday_return >= config.intraday_return_gte
        and features.close_location >= config.close_location_gte
        and features.volume_ratio_20 >= config.volume_ratio_20_gte
        and features.pre_volatility_20 <= config.pre_volatility_20_lte
    )


def _signal_score(features: _SignalFeatures) -> float:
    return features.gap_return + features.close_location + max(features.intraday_return, 0.0)


def _mean_close(bars: list[_BarPoint]) -> float:
    return sum(bar.close for bar in bars) / len(bars)


def _mean_volume(bars: list[_BarPoint]) -> float:
    return sum(bar.volume for bar in bars) / len(bars)


def _pre_volatility_20(bars: list[_BarPoint], index: int) -> float | None:
    if index < 21:
        return None
    returns = []
    for current_index in range(index - 20, index):
        previous_close = bars[current_index - 1].close
        if previous_close <= 0.0:
            return None
        returns.append(bars[current_index].close / previous_close - 1.0)
    return stdev(returns) * sqrt(252.0)
