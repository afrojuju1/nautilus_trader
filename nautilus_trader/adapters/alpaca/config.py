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

from typing import Literal

from nautilus_trader.common.config import PositiveInt
from nautilus_trader.config import LiveDataClientConfig
from nautilus_trader.config import LiveExecClientConfig


AlpacaEnvironment = Literal["paper", "live"]
AlpacaOptionFeed = Literal["indicative", "opra"]
AlpacaStockFeed = Literal["iex", "sip", "delayed_sip"]


class AlpacaDataClientConfig(LiveDataClientConfig, frozen=True):
    """
    Configuration for planned ``AlpacaDataClient`` instances.

    Parameters
    ----------
    api_key : str, optional
        The Alpaca API key.
        If ``None`` then will source the `APCA_API_KEY_ID` or `ALPACA_API_KEY`
        environment variable.
    api_secret : str, optional
        The Alpaca API secret.
        If ``None`` then will source the `APCA_API_SECRET_KEY` or `ALPACA_SECRET_KEY`
        environment variable.
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
    max_option_subscriptions : PositiveInt, default 1000
        Maximum option symbols to subscribe in one WebSocket request.
    request_timeout_secs : PositiveInt, default 30
        HTTP request timeout in seconds.
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
    max_option_subscriptions: PositiveInt = 1_000
    request_timeout_secs: PositiveInt = 30
    snapshot_greeks_poll_secs: PositiveInt | None = None


class AlpacaExecClientConfig(LiveExecClientConfig, frozen=True):
    """
    Configuration for planned ``AlpacaExecutionClient`` instances.

    Parameters
    ----------
    api_key : str, optional
        The Alpaca API key.
        If ``None`` then will source the `APCA_API_KEY_ID` or `ALPACA_API_KEY`
        environment variable.
    api_secret : str, optional
        The Alpaca API secret.
        If ``None`` then will source the `APCA_API_SECRET_KEY` or `ALPACA_SECRET_KEY`
        environment variable.
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
