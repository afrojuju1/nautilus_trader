#!/usr/bin/env python3
"""
Run an Alpaca option quote/Greeks and optional multi-leg order-list node.
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
from nautilus_trader.config import StrategyConfig
from nautilus_trader.config import TradingNodeConfig
from nautilus_trader.live.node import TradingNode
from nautilus_trader.model.data import OptionGreeks
from nautilus_trader.model.data import QuoteTick
from nautilus_trader.model.enums import OrderSide
from nautilus_trader.model.enums import TimeInForce
from nautilus_trader.model.identifiers import InstrumentId
from nautilus_trader.model.identifiers import TraderId
from nautilus_trader.model.instruments import Instrument
from nautilus_trader.trading.strategy import Strategy


DEFAULT_SHORT_SYMBOL = "SPY270115P00450000"
DEFAULT_LONG_SYMBOL = "SPY270115P00445000"


class AlpacaOptionsMlegExampleConfig(StrategyConfig, frozen=True):
    """
    Configuration for the Alpaca option mleg example strategy.
    """

    short_leg_id: InstrumentId
    long_leg_id: InstrumentId
    quantity: int = 1
    short_leg_limit: Decimal = Decimal("0.50")
    long_leg_limit: Decimal = Decimal("0.10")
    submit: bool = False


class AlpacaOptionsMlegExample(Strategy):
    """
    Subscribes option snapshots and can submit one opening two-leg order list.
    """

    def __init__(self, config: AlpacaOptionsMlegExampleConfig) -> None:
        if config.quantity <= 0:
            raise ValueError("quantity must be positive")
        if config.short_leg_limit <= 0 or config.long_leg_limit <= 0:
            raise ValueError("leg limits must be positive")
        super().__init__(config)
        self._instruments: dict[InstrumentId, Instrument] = {}
        self._submitted = False

    def on_start(self) -> None:
        for instrument_id in (self.config.short_leg_id, self.config.long_leg_id):
            self.subscribe_instrument(instrument_id)
            self.subscribe_quote_ticks(instrument_id)
            self.subscribe_option_greeks(instrument_id)

    def on_instrument(self, instrument: Instrument) -> None:
        self._instruments[instrument.id] = instrument
        self._try_submit_order_list()

    def on_quote_tick(self, tick: QuoteTick) -> None:
        self.log.info(
            f"Quote {tick.instrument_id} bid={tick.bid_price} ask={tick.ask_price}",
        )

    def on_option_greeks(self, option_greeks: OptionGreeks) -> None:
        self.log.info(
            f"Greeks {option_greeks.instrument_id} "
            f"delta={option_greeks.delta:.4f} iv={option_greeks.mark_iv}",
        )

    def _try_submit_order_list(self) -> None:
        if not self.config.submit or self._submitted:
            return
        short_leg = self._instruments.get(self.config.short_leg_id)
        long_leg = self._instruments.get(self.config.long_leg_id)
        if short_leg is None or long_leg is None:
            return

        orders = [
            self.order_factory.limit(
                instrument_id=short_leg.id,
                order_side=OrderSide.SELL,
                quantity=short_leg.make_qty(self.config.quantity),
                price=short_leg.make_price(self.config.short_leg_limit),
                time_in_force=TimeInForce.DAY,
            ),
            self.order_factory.limit(
                instrument_id=long_leg.id,
                order_side=OrderSide.BUY,
                quantity=long_leg.make_qty(self.config.quantity),
                price=long_leg.make_price(self.config.long_leg_limit),
                time_in_force=TimeInForce.DAY,
            ),
        ]
        self.submit_order_list(self.order_factory.create_list(orders))
        self._submitted = True


def build_node(
    *,
    short_symbol: str,
    long_symbol: str,
    quantity: int,
    short_leg_limit: Decimal,
    long_leg_limit: Decimal,
    use_broker_paper: bool,
    confirm_submit: bool,
) -> TradingNode:
    symbols = [short_symbol.upper(), long_symbol.upper()]
    instrument_ids = [InstrumentId.from_str(f"{symbol}.{ALPACA}") for symbol in symbols]
    instrument_provider = InstrumentProviderConfig(load_ids=frozenset(instrument_ids))
    routing = RoutingConfig(venues=frozenset([ALPACA]))

    exec_config = (
        AlpacaExecClientConfig(
            environment="paper",
            trading_base_url=os.getenv("ALPACA_TRADING_BASE_URL"),
            instrument_provider=instrument_provider,
            routing=routing,
            use_trade_updates_stream=False,
            reconciliation_poll_secs=60,
            risk_kill_switch=_bool_from_env("ALPACA_OPTIONS_KILL_SWITCH", default=False),
        )
        if use_broker_paper
        else SandboxExecutionClientConfig(
            venue=ALPACA,
            starting_balances=[os.getenv("ALPACA_OPTIONS_SANDBOX_BALANCE", "100000 USD")],
            base_currency="USD",
            account_type="MARGIN",
            oms_type="NETTING",
            instrument_provider=instrument_provider,
            routing=routing,
        )
    )
    exec_factory = AlpacaLiveExecClientFactory if use_broker_paper else SandboxLiveExecClientFactory

    config_node = TradingNodeConfig(
        trader_id=TraderId(os.getenv("ALPACA_OPTIONS_NODE_TRADER_ID", "ALPOPT-001")),
        logging=LoggingConfig(log_level=os.getenv("NAUTILUS_LOG_LEVEL", "INFO"), use_pyo3=True),
        exec_engine=LiveExecEngineConfig(reconciliation=False),
        data_clients={
            ALPACA: AlpacaDataClientConfig(
                option_symbols=symbols,
                option_feed=os.getenv("ALPACA_OPTION_FEED", "indicative"),
                snapshot_greeks_poll_secs=_int_from_env(
                    "ALPACA_OPTION_SNAPSHOT_POLL_SECS",
                    default=60,
                ),
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
        AlpacaOptionsMlegExample(
            AlpacaOptionsMlegExampleConfig(
                short_leg_id=instrument_ids[0],
                long_leg_id=instrument_ids[1],
                quantity=quantity,
                short_leg_limit=short_leg_limit,
                long_leg_limit=long_leg_limit,
                submit=confirm_submit,
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
        "--short-symbol",
        default=os.getenv("ALPACA_OPTION_SHORT_SYMBOL", DEFAULT_SHORT_SYMBOL),
    )
    parser.add_argument(
        "--long-symbol",
        default=os.getenv("ALPACA_OPTION_LONG_SYMBOL", DEFAULT_LONG_SYMBOL),
    )
    parser.add_argument("--qty", type=int, default=_int_from_env("ALPACA_OPTION_QTY", default=1))
    parser.add_argument(
        "--short-leg-limit",
        type=Decimal,
        default=_decimal_from_env("ALPACA_OPTION_SHORT_LEG_LIMIT", default="0.50"),
    )
    parser.add_argument(
        "--long-leg-limit",
        type=Decimal,
        default=_decimal_from_env("ALPACA_OPTION_LONG_LEG_LIMIT", default="0.10"),
    )
    parser.add_argument(
        "--run-seconds",
        type=float,
        default=_float_from_env("ALPACA_OPTIONS_NODE_RUN_SECONDS"),
    )
    parser.add_argument(
        "--broker-paper",
        action="store_true",
        help="Route the optional order list to the Alpaca paper broker.",
    )
    parser.add_argument(
        "--confirm-submit",
        action="store_true",
        help="Submit one opening two-leg option order list. Requires --broker-paper.",
    )
    add_alpaca_profile_args(parser)
    args = parser.parse_args()
    if args.confirm_submit and not args.broker_paper:
        raise ValueError("--confirm-submit requires --broker-paper")

    load_alpaca_profile_from_args(args)
    node = build_node(
        short_symbol=args.short_symbol,
        long_symbol=args.long_symbol,
        quantity=args.qty,
        short_leg_limit=args.short_leg_limit,
        long_leg_limit=args.long_leg_limit,
        use_broker_paper=args.broker_paper,
        confirm_submit=args.confirm_submit,
    )
    if args.check_config:
        node.dispose()
        return
    if args.run_seconds is not None:
        if args.run_seconds <= 0:
            raise ValueError("--run-seconds or ALPACA_OPTIONS_NODE_RUN_SECONDS must be positive")
        node.get_event_loop().call_later(args.run_seconds, node.stop)

    try:
        node.run()
    finally:
        node.dispose()


def _decimal_from_env(name: str, default: str | None = None) -> Decimal | None:
    raw = os.getenv(name, default)
    return Decimal(raw) if raw is not None and raw.strip() else None


def _float_from_env(name: str) -> float | None:
    raw = os.getenv(name)
    return float(raw) if raw is not None and raw.strip() else None


def _int_from_env(name: str, *, default: int) -> int:
    raw = os.getenv(name)
    return int(raw) if raw is not None and raw.strip() else default


def _bool_from_env(name: str, *, default: bool) -> bool:
    raw = os.getenv(name)
    if raw is None:
        return default
    return raw.strip().lower() in {"1", "true", "yes", "on"}


if __name__ == "__main__":
    main()
