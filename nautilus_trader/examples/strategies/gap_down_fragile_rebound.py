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
Daily-bar ETF rebound strategy migrated from ``gap_down_fragile_rebound_v2``.
"""

from __future__ import annotations

from collections import deque
from dataclasses import dataclass
from decimal import ROUND_FLOOR
from decimal import Decimal
from math import ceil
from math import floor
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


STRATEGY_TAG = "gap_down_fragile_rebound_v2"


class GapDownFragileReboundConfig(StrategyConfig, frozen=True):
    """
    Configuration for ``GapDownFragileRebound``.
    """

    bar_types: list[BarType]
    strategy_capital: Decimal
    capital_fraction: float = 1.0
    request_historical_bars: bool = True
    history_days: PositiveInt = 1300
    calibration_lookback_sessions: PositiveInt = 756
    min_history_sessions: PositiveInt = 756
    default_hold_bars: PositiveInt = 10
    extended_hold_bars: PositiveInt = 15
    extended_hold_rsi_threshold: float = 43.871608
    limit_offset_bps: float = 5.0
    entry_fill_timeout_bars: PositiveInt = 2
    gap_return_gte: float = -0.04
    gap_return_lte: float = -0.01
    intraday_return_gte: float = -0.005
    close_location_gte: float = 0.4
    close_location_lte: float = 0.6
    gap_fill_pct_gte: float = 0.1
    fragile_drawdown_quantile: float = 0.4
    high_volatility_quantile: float = 0.67


@dataclass(frozen=True)
class _BarPoint:
    open: float
    high: float
    low: float
    close: float


@dataclass(frozen=True)
class _SignalFeatures:
    gap_return: float
    intraday_return: float
    close_location: float
    gap_fill_pct: float
    pre_drawdown_20: float
    pre_volatility_20: float
    pre_rsi_14: float


@dataclass(frozen=True)
class _SignalCandidate:
    bar_type: BarType
    instrument_id: InstrumentId
    close: float
    score: float
    exit_after_bars: int
    features: _SignalFeatures


@dataclass
class _ActiveTrade:
    bar_type: BarType
    instrument_id: InstrumentId
    quantity: Decimal
    exit_after_bars: int
    bars_held: int = 0
    entry_wait_bars: int = 0
    exit_submitted: bool = False


class GapDownFragileRebound(Strategy):
    """
    Regular Nautilus strategy for the ETF gap-down fragile rebound book.
    """

    def __init__(self, config: GapDownFragileReboundConfig) -> None:
        if not config.bar_types:
            raise ValueError("bar_types must not be empty")
        if config.strategy_capital <= Decimal(0):
            raise ValueError("strategy_capital must be positive")
        if not 0.0 < config.capital_fraction <= 1.0:
            raise ValueError("capital_fraction must be in the range (0, 1]")
        if config.limit_offset_bps < 0.0:
            raise ValueError("limit_offset_bps must be non-negative")
        if config.default_hold_bars > config.extended_hold_bars:
            raise ValueError("default_hold_bars must be <= extended_hold_bars")
        super().__init__(config)

        max_history = (
            config.calibration_lookback_sessions
            + config.extended_hold_bars
            + 40
        )
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
        if len(history) < self.config.min_history_sessions:
            return None
        if bar.is_single_price():
            return None

        point = _bar_point(bar)
        features = _features_for_index(
            [*history, point],
            len(history),
            calibration_lookback_sessions=self.config.calibration_lookback_sessions,
            fragile_drawdown_quantile=self.config.fragile_drawdown_quantile,
            high_volatility_quantile=self.config.high_volatility_quantile,
        )
        if features is None or not _passes_signal(features, self.config):
            return None

        score = _trailing_forward_return_score(
            list(history),
            config=self.config,
        )
        exit_after_bars = (
            self.config.extended_hold_bars
            if features.pre_rsi_14 <= self.config.extended_hold_rsi_threshold
            else self.config.default_hold_bars
        )
        return _SignalCandidate(
            bar_type=bar_type,
            instrument_id=bar_type.instrument_id,
            close=point.close,
            score=score,
            exit_after_bars=exit_after_bars,
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
                candidate.features.gap_fill_pct,
                -abs(candidate.features.gap_return),
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
            exit_after_bars=candidate.exit_after_bars,
        )
        self.log.info(
            f"{STRATEGY_TAG} entry submitted {candidate.instrument_id} "
            f"qty={quantity_decimal} score={candidate.score:.6f} "
            f"hold={candidate.exit_after_bars}",
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
        if active.bars_held >= active.exit_after_bars:
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
        shares = (capital / Decimal(str(price))).to_integral_value(rounding=ROUND_FLOOR)
        return shares


def _bar_point(bar: Bar) -> _BarPoint:
    return _BarPoint(
        open=bar.open.as_double(),
        high=bar.high.as_double(),
        low=bar.low.as_double(),
        close=bar.close.as_double(),
    )


def _features_for_index(
    bars: list[_BarPoint],
    index: int,
    *,
    calibration_lookback_sessions: int,
    fragile_drawdown_quantile: float,
    high_volatility_quantile: float,
) -> _SignalFeatures | None:
    if index < 21:
        return None

    current = bars[index]
    previous = bars[index - 1]
    if previous.close <= 0.0 or current.open <= 0.0:
        return None

    high_low_range = current.high - current.low
    if high_low_range <= 0.0:
        return None
    gap_fill_denominator = previous.close - current.open
    if gap_fill_denominator <= 0.0:
        return None

    pre_drawdown = _pre_drawdown_20(bars, index)
    pre_volatility = _pre_volatility_20(bars, index)
    pre_rsi = _rsi_14_at_previous_close(bars, index)
    if pre_drawdown is None or pre_volatility is None or pre_rsi is None:
        return None

    context_metrics = _context_metrics_before(bars, index)
    recent_context = context_metrics[-calibration_lookback_sessions:]
    if len(recent_context) < min(calibration_lookback_sessions, 20):
        return None

    drawdown_threshold = _quantile(
        [metric[0] for metric in recent_context],
        fragile_drawdown_quantile,
    )
    volatility_threshold = _quantile(
        [metric[1] for metric in recent_context],
        high_volatility_quantile,
    )
    fragile_or_stress = (
        pre_drawdown <= drawdown_threshold
        or pre_volatility >= volatility_threshold
    )
    if not fragile_or_stress:
        return None

    return _SignalFeatures(
        gap_return=current.open / previous.close - 1.0,
        intraday_return=current.close / current.open - 1.0,
        close_location=(current.close - current.low) / high_low_range,
        gap_fill_pct=(current.close - current.open) / gap_fill_denominator,
        pre_drawdown_20=pre_drawdown,
        pre_volatility_20=pre_volatility,
        pre_rsi_14=pre_rsi,
    )


def _passes_signal(features: _SignalFeatures, config: GapDownFragileReboundConfig) -> bool:
    if not config.gap_return_gte <= features.gap_return <= config.gap_return_lte:
        return False
    if features.intraday_return < config.intraday_return_gte:
        return False
    if not config.close_location_gte <= features.close_location <= config.close_location_lte:
        return False
    if features.gap_fill_pct < config.gap_fill_pct_gte:
        return False

    strong_reversal = (
        features.intraday_return >= 0.0
        and features.close_location >= 0.6
        and features.gap_fill_pct >= 0.25
    )
    return not strong_reversal


def _trailing_forward_return_score(
    history: list[_BarPoint],
    *,
    config: GapDownFragileReboundConfig,
) -> float:
    if len(history) < config.min_history_sessions:
        return 0.0

    start = max(21, len(history) - config.calibration_lookback_sessions)
    returns: list[float] = []
    for index in range(start, len(history) - 5):
        features = _features_for_index(
            history,
            index,
            calibration_lookback_sessions=config.calibration_lookback_sessions,
            fragile_drawdown_quantile=config.fragile_drawdown_quantile,
            high_volatility_quantile=config.high_volatility_quantile,
        )
        if features is None or not _passes_signal(features, config):
            continue

        entry_close = history[index].close
        exit_close = history[index + 5].close
        if entry_close > 0.0:
            returns.append(exit_close / entry_close - 1.0)

    if not returns:
        return 0.0
    return sum(returns) / len(returns)


def _context_metrics_before(bars: list[_BarPoint], index: int) -> list[tuple[float, float]]:
    metrics: list[tuple[float, float]] = []
    for context_index in range(21, index):
        drawdown = _pre_drawdown_20(bars, context_index)
        volatility = _pre_volatility_20(bars, context_index)
        if drawdown is not None and volatility is not None:
            metrics.append((drawdown, volatility))
    return metrics


def _pre_drawdown_20(bars: list[_BarPoint], index: int) -> float | None:
    if index < 20:
        return None
    prior_closes = [bar.close for bar in bars[index - 20 : index]]
    rolling_max = max(prior_closes)
    if rolling_max <= 0.0:
        return None
    return bars[index - 1].close / rolling_max - 1.0


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


def _rsi_14_at_previous_close(bars: list[_BarPoint], index: int) -> float | None:
    if index < 15:
        return None
    gains: list[float] = []
    losses: list[float] = []
    for current_index in range(index - 14, index):
        delta = bars[current_index].close - bars[current_index - 1].close
        gains.append(max(delta, 0.0))
        losses.append(max(-delta, 0.0))

    average_gain = sum(gains) / 14.0
    average_loss = sum(losses) / 14.0
    if average_loss == 0.0:
        return 100.0
    relative_strength = average_gain / average_loss
    return 100.0 - (100.0 / (1.0 + relative_strength))


def _quantile(values: list[float], q: float) -> float:
    if not values:
        raise ValueError("values must not be empty")
    if not 0.0 <= q <= 1.0:
        raise ValueError("q must be in the range [0, 1]")

    ordered = sorted(values)
    position = (len(ordered) - 1) * q
    lower = floor(position)
    upper = ceil(position)
    if lower == upper:
        return ordered[int(position)]
    lower_value = ordered[lower]
    upper_value = ordered[upper]
    weight = position - lower
    return lower_value + (upper_value - lower_value) * weight
