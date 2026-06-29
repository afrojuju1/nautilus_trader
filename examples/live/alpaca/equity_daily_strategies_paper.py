#!/usr/bin/env python3
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
Run migrated Alpaca equity daily-bar strategies through one Python TradingNode.
"""

from __future__ import annotations

import argparse
import os
from decimal import Decimal

from nautilus_trader.adapters.alpaca import ALPACA
from nautilus_trader.adapters.alpaca import AlpacaDataClientConfig
from nautilus_trader.adapters.alpaca import AlpacaExecClientConfig
from nautilus_trader.adapters.alpaca import AlpacaLiveDataClientFactory
from nautilus_trader.adapters.alpaca import AlpacaLiveExecClientFactory
from nautilus_trader.adapters.alpaca import add_alpaca_profile_args
from nautilus_trader.adapters.alpaca import load_alpaca_profile_from_args
from nautilus_trader.adapters.sandbox.config import SandboxExecutionClientConfig
from nautilus_trader.adapters.sandbox.factory import SandboxLiveExecClientFactory
from nautilus_trader.config import InstrumentProviderConfig
from nautilus_trader.config import LiveExecEngineConfig
from nautilus_trader.config import LoggingConfig
from nautilus_trader.config import RoutingConfig
from nautilus_trader.config import TradingNodeConfig
from nautilus_trader.examples.strategies.gap_down_fragile_rebound import GapDownFragileRebound
from nautilus_trader.examples.strategies.gap_down_fragile_rebound import GapDownFragileReboundConfig
from nautilus_trader.examples.strategies.upside_gap_continuation import UpsideGapContinuation
from nautilus_trader.examples.strategies.upside_gap_continuation import UpsideGapContinuationConfig
from nautilus_trader.live.node import TradingNode
from nautilus_trader.model.data import BarType
from nautilus_trader.model.identifiers import InstrumentId
from nautilus_trader.model.identifiers import TraderId


GAP_REBOUND_SYMBOLS = ["SPY", "QQQ", "IWM", "DIA", "GLD"]
UPSIDE_GAP_SYMBOLS = ["FXI", "SMH", "SOXX"]


def build_node(use_broker_paper: bool = False) -> TradingNode:
    gap_symbols = _symbols_from_env("ALPACA_GAP_REBOUND_SYMBOLS", GAP_REBOUND_SYMBOLS)
    upside_symbols = _symbols_from_env("ALPACA_UPSIDE_GAP_SYMBOLS", UPSIDE_GAP_SYMBOLS)
    symbols = _dedupe_symbols([*gap_symbols, *upside_symbols])
    instrument_ids = [InstrumentId.from_str(f"{symbol}.{ALPACA}") for symbol in symbols]
    instrument_provider = InstrumentProviderConfig(load_ids=frozenset(instrument_ids))
    routing = RoutingConfig(venues=frozenset([ALPACA]))

    gap_capital = Decimal(os.getenv("ALPACA_GAP_REBOUND_CAPITAL", "10000"))
    upside_capital = Decimal(os.getenv("ALPACA_UPSIDE_GAP_CAPITAL", "10000"))
    total_capital = gap_capital + upside_capital
    max_strategy_capital = max(gap_capital, upside_capital)

    exec_config = (
        AlpacaExecClientConfig(
            environment="paper",
            trading_base_url=os.getenv("ALPACA_TRADING_BASE_URL"),
            instrument_provider=instrument_provider,
            routing=routing,
            use_trade_updates_stream=False,
            reconciliation_poll_secs=60,
            risk_kill_switch=_bool_from_env("ALPACA_EQUITY_KILL_SWITCH", default=False),
            max_order_notional=_decimal_from_env(
                "ALPACA_EQUITY_MAX_ORDER_NOTIONAL",
                default=str(max_strategy_capital),
            ),
            max_total_notional=_decimal_from_env(
                "ALPACA_EQUITY_MAX_TOTAL_NOTIONAL",
                default=str(total_capital * Decimal(2)),
            ),
            max_buying_power_pct=_float_from_env("ALPACA_EQUITY_MAX_BUYING_POWER_PCT"),
            allow_duplicate_symbol_exposure=_bool_from_env(
                "ALPACA_EQUITY_ALLOW_DUPLICATE_SYMBOL_EXPOSURE",
                default=False,
            ),
            allow_short_selling=_bool_from_env(
                "ALPACA_EQUITY_ALLOW_SHORT_SELLING",
                default=False,
            ),
        )
        if use_broker_paper
        else SandboxExecutionClientConfig(
            venue=ALPACA,
            starting_balances=[f"{total_capital * Decimal(2)} USD"],
            base_currency="USD",
            account_type="CASH",
            oms_type="NETTING",
            instrument_provider=instrument_provider,
            routing=routing,
            bar_execution=True,
            trade_execution=False,
        )
    )
    exec_factory = AlpacaLiveExecClientFactory if use_broker_paper else SandboxLiveExecClientFactory

    config_node = TradingNodeConfig(
        trader_id=TraderId(os.getenv("ALPACA_EQUITY_DAILY_TRADER_ID", "EQDAY-001")),
        logging=LoggingConfig(log_level=os.getenv("NAUTILUS_LOG_LEVEL", "INFO"), use_pyo3=True),
        exec_engine=LiveExecEngineConfig(reconciliation=False),
        data_clients={
            ALPACA: AlpacaDataClientConfig(
                equity_symbols=symbols,
                stock_feed=os.getenv("ALPACA_STOCK_FEED", "iex"),
                instrument_provider=instrument_provider,
                routing=routing,
            ),
        },
        exec_clients={ALPACA: exec_config},
        timeout_connection=30.0,
        timeout_reconciliation=5.0,
        timeout_portfolio=5.0,
        timeout_disconnection=10.0,
        timeout_post_stop=2.0,
    )

    node = TradingNode(config=config_node)
    node.trader.add_strategy(
        GapDownFragileRebound(
            GapDownFragileReboundConfig(
                bar_types=_bar_types_for_symbols(gap_symbols),
                strategy_capital=gap_capital,
            ),
        ),
    )
    node.trader.add_strategy(
        UpsideGapContinuation(
            UpsideGapContinuationConfig(
                bar_types=_bar_types_for_symbols(upside_symbols),
                strategy_capital=upside_capital,
            ),
        ),
    )
    node.add_data_client_factory(ALPACA, AlpacaLiveDataClientFactory)
    node.add_exec_client_factory(ALPACA, exec_factory)
    node.build()
    return node


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check-config", action="store_true")
    parser.add_argument(
        "--run-seconds",
        type=float,
        help=(
            "Stop the node gracefully after this many seconds. Defaults to "
            "ALPACA_EQUITY_DAILY_RUN_SECONDS when set."
        ),
    )
    parser.add_argument(
        "--broker-paper",
        action="store_true",
        help="Route orders to the Alpaca paper broker instead of Nautilus sandbox execution.",
    )
    add_alpaca_profile_args(parser)
    args = parser.parse_args()

    load_alpaca_profile_from_args(args)
    node = build_node(use_broker_paper=args.broker_paper)
    if args.check_config:
        node.dispose()
        return
    run_seconds = (
        args.run_seconds
        if args.run_seconds is not None
        else _float_from_env("ALPACA_EQUITY_DAILY_RUN_SECONDS")
    )
    if run_seconds is not None:
        if run_seconds <= 0:
            raise ValueError("--run-seconds or ALPACA_EQUITY_DAILY_RUN_SECONDS must be positive")
        node.get_event_loop().call_later(run_seconds, node.stop)

    try:
        node.run()
    finally:
        node.dispose()


def _bar_types_for_symbols(symbols: list[str]) -> list[BarType]:
    return [BarType.from_str(f"{symbol}.{ALPACA}-1-DAY-LAST-EXTERNAL") for symbol in symbols]


def _symbols_from_env(name: str, default: list[str]) -> list[str]:
    raw = os.getenv(name)
    if raw is None:
        return default
    return [symbol.strip().upper() for symbol in raw.split(",") if symbol.strip()]


def _dedupe_symbols(symbols: list[str]) -> list[str]:
    return list(dict.fromkeys(symbols))


def _decimal_from_env(name: str, default: str | None = None) -> Decimal | None:
    raw = os.getenv(name, default)
    return Decimal(raw) if raw is not None and raw.strip() else None


def _float_from_env(name: str) -> float | None:
    raw = os.getenv(name)
    return float(raw) if raw is not None and raw.strip() else None


def _bool_from_env(name: str, *, default: bool) -> bool:
    raw = os.getenv(name)
    if raw is None:
        return default
    return raw.strip().lower() in {"1", "true", "yes", "on"}


if __name__ == "__main__":
    main()
