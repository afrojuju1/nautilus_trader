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
Run GapDownFragileRebound through a Python TradingNode with Alpaca data and sandbox execution.

The strategy submits regular Nautilus orders. This example routes those orders to the Nautilus
sandbox execution client, not to the Alpaca broker-paper account.
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
from nautilus_trader.adapters.alpaca.strategies import GapDownFragileRebound
from nautilus_trader.adapters.alpaca.strategies import GapDownFragileReboundConfig
from nautilus_trader.adapters.sandbox.config import SandboxExecutionClientConfig
from nautilus_trader.adapters.sandbox.factory import SandboxLiveExecClientFactory
from nautilus_trader.config import InstrumentProviderConfig
from nautilus_trader.config import LiveExecEngineConfig
from nautilus_trader.config import LoggingConfig
from nautilus_trader.config import RoutingConfig
from nautilus_trader.config import TradingNodeConfig
from nautilus_trader.live.node import TradingNode
from nautilus_trader.model.data import BarType
from nautilus_trader.model.identifiers import InstrumentId
from nautilus_trader.model.identifiers import TraderId


ETF_SYMBOLS = ["SPY", "QQQ", "IWM", "DIA", "GLD"]


def build_node(use_broker_paper: bool = False) -> TradingNode:
    symbols = _symbols_from_env()
    instrument_ids = [InstrumentId.from_str(f"{symbol}.{ALPACA}") for symbol in symbols]
    bar_types = [
        BarType.from_str(f"{instrument_id}-1-DAY-LAST-EXTERNAL") for instrument_id in instrument_ids
    ]
    strategy_capital = Decimal(os.getenv("ALPACA_GAP_REBOUND_CAPITAL", "10000"))
    instrument_provider = InstrumentProviderConfig(load_ids=frozenset(instrument_ids))
    routing = RoutingConfig(venues=frozenset([ALPACA]))

    exec_config = (
        AlpacaExecClientConfig(
            environment="paper",
            instrument_provider=instrument_provider,
            routing=routing,
            use_trade_updates_stream=False,
            reconciliation_poll_secs=60,
        )
        if use_broker_paper
        else SandboxExecutionClientConfig(
            venue=ALPACA,
            starting_balances=[f"{strategy_capital * Decimal(2)} USD"],
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
        trader_id=TraderId(os.getenv("ALPACA_GAP_REBOUND_TRADER_ID", "GAPREB-001")),
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
                bar_types=bar_types,
                strategy_capital=strategy_capital,
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
        "--broker-paper",
        action="store_true",
        help="Route orders to the Alpaca paper broker instead of Nautilus sandbox execution.",
    )
    args = parser.parse_args()

    node = build_node(use_broker_paper=args.broker_paper)
    if args.check_config:
        node.dispose()
        return

    try:
        node.run()
    finally:
        node.dispose()


def _symbols_from_env() -> list[str]:
    raw = os.getenv("ALPACA_GAP_REBOUND_SYMBOLS")
    if raw is None:
        return ETF_SYMBOLS
    return [symbol.strip().upper() for symbol in raw.split(",") if symbol.strip()]


if __name__ == "__main__":
    main()
