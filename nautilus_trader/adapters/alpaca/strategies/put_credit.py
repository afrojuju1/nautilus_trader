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
Timer scaffold for Alpaca put credit spread selection.

This strategy calls the Alpaca Rust scanner through the Nautilus PyO3 extension. It emits candidate
decisions only; order submission stays in the execution client/harness until the full live adapter
is wired.
"""

from __future__ import annotations

from datetime import datetime
from datetime import time
from datetime import timedelta
from zoneinfo import ZoneInfo

import msgspec

from nautilus_trader.common.config import PositiveInt
from nautilus_trader.common.events import TimeEvent
from nautilus_trader.config import StrategyConfig
from nautilus_trader.trading.strategy import Strategy


SCAN_TIMER_NAME = "alpaca_put_credit_scan"


def _default_underlyings() -> list[str]:
    return ["SPY", "QQQ", "IWM", "DIA", "GLD"]


class AlpacaPutCreditStrategyConfig(StrategyConfig, frozen=True):
    """
    Configuration for ``AlpacaPutCreditStrategy``.
    """

    underlyings: list[str] = msgspec.field(default_factory=_default_underlyings)
    scan_interval_secs: PositiveInt = 300
    entry_start_time: str = "09:45"
    entry_end_time: str = "14:30"
    entry_timezone: str = "America/New_York"
    min_dte: PositiveInt = 5
    max_dte: PositiveInt = 10
    short_delta_min: float = 0.18
    short_delta_max: float = 0.28
    widths: list[float] = msgspec.field(default_factory=lambda: [2.0, 3.0, 5.0])
    min_open_interest: PositiveInt = 200
    max_leg_spread_pct: float = 0.15
    min_return_on_risk: float = 0.13


class AlpacaPutCreditStrategy(Strategy):
    """
    Nautilus timer loop for the Alpaca put credit scanner.
    """

    def __init__(self, config: AlpacaPutCreditStrategyConfig) -> None:
        super().__init__(config)
        self._entry_tz = ZoneInfo(config.entry_timezone)
        self._entry_start = _parse_hhmm(config.entry_start_time)
        self._entry_end = _parse_hhmm(config.entry_end_time)

    def on_start(self) -> None:
        """
        Start the scanner timer.
        """
        self.clock.set_timer(
            name=SCAN_TIMER_NAME,
            interval=timedelta(seconds=self.config.scan_interval_secs),
            callback=self.on_time_event,
        )
        self._run_scan("start")

    def on_stop(self) -> None:
        """
        Stop the scanner timer.
        """
        if SCAN_TIMER_NAME in self.clock.timer_names:
            self.clock.cancel_timer(SCAN_TIMER_NAME)

    def on_time_event(self, event: TimeEvent) -> None:
        """
        Run the scanner on each timer event.
        """
        self._run_scan(event.name)

    def _run_scan(self, trigger: str) -> None:
        if not self._inside_entry_window():
            self.log.debug(f"Skipping Alpaca put credit scan outside entry window: {trigger=}")
            return

        try:
            from nautilus_trader.core.nautilus_pyo3.alpaca import AlpacaPutCreditScannerConfig
            from nautilus_trader.core.nautilus_pyo3.alpaca import scan_put_credit_once

            scanner_config = AlpacaPutCreditScannerConfig(
                min_dte=self.config.min_dte,
                max_dte=self.config.max_dte,
                short_delta_min=self.config.short_delta_min,
                short_delta_max=self.config.short_delta_max,
                widths=self.config.widths,
                min_open_interest=self.config.min_open_interest,
                max_leg_spread_pct=self.config.max_leg_spread_pct,
                min_return_on_risk=self.config.min_return_on_risk,
            )
            results = scan_put_credit_once(self.config.underlyings, scanner_config)
        except RuntimeError as exc:
            self.log.error(f"Alpaca put credit scanner failed: {exc}")
            return

        for result in results:
            if result.candidates:
                best = result.candidates[0]
                self.log.info(
                    f"{result.underlying}: entry_ready short={best.short_symbol} "
                    f"long={best.long_symbol} credit={best.credit:.2f} "
                    f"ror={best.return_on_risk * 100.0:.1f}% score={best.score:.1f}",
                )
            else:
                self.log.info(
                    f"{result.underlying}: no_candidate contracts={result.contract_count} "
                    f"snapshots={result.snapshot_count} scoreable={result.scoreable_count}",
                )

    def _inside_entry_window(self) -> bool:
        now = datetime.now(tz=self._entry_tz).time()
        return self._entry_start <= now <= self._entry_end


def _parse_hhmm(value: str) -> time:
    hour, minute = value.split(":", maxsplit=1)
    return time(hour=int(hour), minute=int(minute))
