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
Alpaca Markets integration package for NautilusTrader.

The Rust Alpaca options runtime is implemented in ``nautilus-alpaca`` and exposed through
diagnostic/operator binaries plus selected PyO3 bindings. The Python ``TradingNode`` factories
support static US equity instruments, exact OCC option instruments, stock bars, option snapshot
quotes/Greeks, simple US equity/ETF DAY limit orders, and option multi-leg order lists.
"""

from nautilus_trader.adapters.alpaca.config import AlpacaDataClientConfig
from nautilus_trader.adapters.alpaca.config import AlpacaExecClientConfig
from nautilus_trader.adapters.alpaca.constants import ALPACA
from nautilus_trader.adapters.alpaca.constants import ALPACA_API_KEY_ENV
from nautilus_trader.adapters.alpaca.constants import ALPACA_CLIENT_ID
from nautilus_trader.adapters.alpaca.constants import ALPACA_SECRET_KEY_ENV
from nautilus_trader.adapters.alpaca.constants import ALPACA_VENUE
from nautilus_trader.adapters.alpaca.constants import APCA_API_KEY_ID_ENV
from nautilus_trader.adapters.alpaca.constants import APCA_API_SECRET_KEY_ENV
from nautilus_trader.adapters.alpaca.data import AlpacaDataClient
from nautilus_trader.adapters.alpaca.execution import AlpacaExecutionClient
from nautilus_trader.adapters.alpaca.factories import AlpacaLiveDataClientFactory
from nautilus_trader.adapters.alpaca.factories import AlpacaLiveExecClientFactory
from nautilus_trader.adapters.alpaca.profiles import add_alpaca_profile_args
from nautilus_trader.adapters.alpaca.profiles import alpaca_env_file_for_profile
from nautilus_trader.adapters.alpaca.profiles import load_alpaca_env_file
from nautilus_trader.adapters.alpaca.profiles import load_alpaca_profile_from_args
from nautilus_trader.adapters.alpaca.providers import AlpacaEquityInstrumentProvider
from nautilus_trader.adapters.alpaca.providers import AlpacaInstrumentProvider
from nautilus_trader.adapters.alpaca.providers import is_alpaca_option_symbol
from nautilus_trader.adapters.alpaca.providers import make_alpaca_equity
from nautilus_trader.adapters.alpaca.providers import make_alpaca_option
from nautilus_trader.adapters.alpaca.strategies import AlpacaPutCreditStrategy
from nautilus_trader.adapters.alpaca.strategies import AlpacaPutCreditStrategyConfig
from nautilus_trader.adapters.alpaca.strategies import UpsideGapContinuation
from nautilus_trader.adapters.alpaca.strategies import UpsideGapContinuationConfig


__all__ = [
    "ALPACA",
    "ALPACA_API_KEY_ENV",
    "ALPACA_CLIENT_ID",
    "ALPACA_SECRET_KEY_ENV",
    "ALPACA_VENUE",
    "APCA_API_KEY_ID_ENV",
    "APCA_API_SECRET_KEY_ENV",
    "AlpacaDataClient",
    "AlpacaDataClientConfig",
    "AlpacaEquityInstrumentProvider",
    "AlpacaExecClientConfig",
    "AlpacaExecutionClient",
    "AlpacaInstrumentProvider",
    "AlpacaLiveDataClientFactory",
    "AlpacaLiveExecClientFactory",
    "AlpacaPutCreditStrategy",
    "AlpacaPutCreditStrategyConfig",
    "UpsideGapContinuation",
    "UpsideGapContinuationConfig",
    "add_alpaca_profile_args",
    "alpaca_env_file_for_profile",
    "is_alpaca_option_symbol",
    "load_alpaca_env_file",
    "load_alpaca_profile_from_args",
    "make_alpaca_equity",
    "make_alpaca_option",
]
