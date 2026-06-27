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
from typing import Literal

import msgspec

from nautilus_trader.common.config import NonNegativeFloat
from nautilus_trader.common.config import PositiveInt
from nautilus_trader.config import LiveDataClientConfig
from nautilus_trader.config import LiveExecClientConfig


AlpacaEnvironment = Literal["paper", "live"]
AlpacaOptionFeed = Literal["indicative", "opra"]
AlpacaStockFeed = Literal["iex", "sip", "delayed_sip"]


class AlpacaDataClientConfig(LiveDataClientConfig, frozen=True):
    """
    Configuration shared by Alpaca market-data utilities and ``AlpacaDataClient`` instances.

    Parameters
    ----------
    api_key : str, optional
        The Alpaca API key.
        If ``None`` then will source the `APCA_API_KEY_ID` or `ALPACA_API_KEY`
        environment variable.
    api_secret : str, optional
        The Alpaca API secret.
        If ``None`` then will source the `APCA_API_SECRET_KEY` or `ALPACA_SECRET_KEY`
        or `ALPACA_API_SECRET` environment variable.
    environment : {"paper", "live"}, default "paper"
        The Alpaca trading environment used when data requests need account context.
    data_base_url : str, optional
        Override for Alpaca market data REST requests.
    trading_base_url : str, optional
        Override for account-scoped trading REST requests.
    stock_feed : {"iex", "sip", "delayed_sip"}, default "iex"
        Stock feed to request.
    option_feed : {"indicative", "opra"}, default "indicative"
        Option feed to request.
    equity_symbols : list[str], default []
        US equity/ETF symbols to model as Alpaca equity instruments for the Python
        ``TradingNode`` data client.
    option_symbols : list[str], default []
        Exact Alpaca/OCC option symbols to model as option instruments for the Python
        ``TradingNode`` data client.
    request_timeout_secs : PositiveInt, default 30
        HTTP request timeout in seconds.
    bar_poll_interval_secs : PositiveInt, default 300
        Poll interval for externally aggregated stock bars.
    bars_timestamp_on_close : bool, default True
        If bar timestamps should be shifted to the close of the aggregation interval.
    snapshot_greeks_poll_secs : PositiveInt, optional
        Optional polling interval for option snapshot Greeks and IV.

    """

    api_key: str | None = None
    api_secret: str | None = None
    environment: AlpacaEnvironment = "paper"
    data_base_url: str | None = None
    trading_base_url: str | None = None
    stock_feed: AlpacaStockFeed = "iex"
    option_feed: AlpacaOptionFeed = "indicative"
    equity_symbols: list[str] = msgspec.field(default_factory=list)
    option_symbols: list[str] = msgspec.field(default_factory=list)
    request_timeout_secs: PositiveInt = 30
    bar_poll_interval_secs: PositiveInt = 300
    bars_timestamp_on_close: bool = True
    snapshot_greeks_poll_secs: PositiveInt | None = None


class AlpacaExecClientConfig(LiveExecClientConfig, frozen=True):
    """
    Configuration shared by the Alpaca Rust execution runtime and ``AlpacaExecutionClient``
    instances.

    Parameters
    ----------
    api_key : str, optional
        The Alpaca API key.
        If ``None`` then will source the `APCA_API_KEY_ID` or `ALPACA_API_KEY`
        environment variable.
    api_secret : str, optional
        The Alpaca API secret.
        If ``None`` then will source the `APCA_API_SECRET_KEY` or `ALPACA_SECRET_KEY`
        or `ALPACA_API_SECRET` environment variable.
    environment : {"paper", "live"}, default "paper"
        The Alpaca trading environment.
    trading_base_url : str, optional
        Override for account and order REST requests.
    trade_updates_ws_url : str, optional
        Override for Alpaca account trade update events.
    request_timeout_secs : PositiveInt, default 30
        HTTP request timeout in seconds.
    use_trade_updates_stream : bool, default True
        If Alpaca trade update events should be consumed over WebSocket.
    client_order_id_prefix : str, default "nautilus"
        Prefix for generated client order IDs.
    external_order_filtering : bool, default False
        If execution reports for client order IDs outside the prefix should be ignored.
    reconciliation_poll_secs : PositiveInt, optional
        Optional interval for REST reconciliation repair polling.
    risk_kill_switch : bool, default False
        If True then all new broker order submissions are denied by the execution client.
    max_order_notional : Decimal, optional
        Maximum notional for any single risk-increasing equity order.
    max_total_notional : Decimal, optional
        Maximum account-level equity notional exposure including open broker positions and orders.
    enforce_buying_power : bool, default True
        If True then risk-increasing orders are denied when their notional exceeds the last
        observed Alpaca buying power.
    allow_duplicate_symbol_exposure : bool, default False
        If False then risk-increasing orders are denied when the account already has an open order
        or position for the same symbol.
    allow_short_selling : bool, default False
        If False then sell orders are only allowed when they can close existing long quantity.
    max_buying_power_pct : NonNegativeFloat, optional
        Maximum fraction of last observed buying power one risk-increasing equity order may use.

    """

    api_key: str | None = None
    api_secret: str | None = None
    environment: AlpacaEnvironment = "paper"
    trading_base_url: str | None = None
    trade_updates_ws_url: str | None = None
    request_timeout_secs: PositiveInt = 30
    use_trade_updates_stream: bool = True
    client_order_id_prefix: str = "nautilus"
    external_order_filtering: bool = False
    reconciliation_poll_secs: PositiveInt | None = 60
    risk_kill_switch: bool = False
    max_order_notional: Decimal | None = None
    max_total_notional: Decimal | None = None
    enforce_buying_power: bool = True
    allow_duplicate_symbol_exposure: bool = False
    allow_short_selling: bool = False
    max_buying_power_pct: NonNegativeFloat | None = None
