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

from nautilus_trader.adapters.alpaca.config import AlpacaDataClientConfig
from nautilus_trader.adapters.alpaca.config import AlpacaExecClientConfig
from nautilus_trader.adapters.alpaca.data import AlpacaDataClient
from nautilus_trader.adapters.alpaca.execution import AlpacaExecutionClient
from nautilus_trader.adapters.alpaca.providers import AlpacaInstrumentProvider
from nautilus_trader.cache.cache import Cache
from nautilus_trader.common.component import LiveClock
from nautilus_trader.common.component import MessageBus
from nautilus_trader.live.factories import LiveDataClientFactory
from nautilus_trader.live.factories import LiveExecClientFactory


class AlpacaLiveDataClientFactory(LiveDataClientFactory):
    """
    Provides an Alpaca data client factory for Python ``TradingNode`` usage.
    """

    @staticmethod
    def create(
        loop,
        name: str,
        config: AlpacaDataClientConfig,
        msgbus: MessageBus,
        cache: Cache,
        clock: LiveClock,
    ):
        instrument_provider = AlpacaInstrumentProvider(
            equity_symbols=config.equity_symbols,
            option_symbols=config.option_symbols,
            config=config.instrument_provider,
        )
        return AlpacaDataClient(
            loop=loop,
            msgbus=msgbus,
            cache=cache,
            clock=clock,
            instrument_provider=instrument_provider,
            config=config,
            name=name,
        )


class AlpacaLiveExecClientFactory(LiveExecClientFactory):
    """
    Provides an Alpaca execution client factory for Python ``TradingNode`` usage.

    The Python client supports simple US equity/ETF DAY limit orders and Alpaca option multi-leg
    order lists.
    """

    @staticmethod
    def create(
        loop,
        name: str,
        config: AlpacaExecClientConfig,
        msgbus: MessageBus,
        cache: Cache,
        clock: LiveClock,
    ):
        instrument_provider = AlpacaInstrumentProvider(
            config=config.instrument_provider,
        )
        return AlpacaExecutionClient(
            loop=loop,
            msgbus=msgbus,
            cache=cache,
            clock=clock,
            instrument_provider=instrument_provider,
            config=config,
            name=name,
        )
