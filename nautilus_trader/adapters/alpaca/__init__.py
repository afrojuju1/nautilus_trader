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
Alpaca Markets adapter scaffold for NautilusTrader.

The package currently exposes configuration and factory placeholders while the Rust
data and execution clients are implemented.
"""

from nautilus_trader.adapters.alpaca.config import AlpacaDataClientConfig
from nautilus_trader.adapters.alpaca.config import AlpacaExecClientConfig
from nautilus_trader.adapters.alpaca.constants import ALPACA
from nautilus_trader.adapters.alpaca.constants import ALPACA_CLIENT_ID
from nautilus_trader.adapters.alpaca.constants import ALPACA_VENUE
from nautilus_trader.adapters.alpaca.factories import AlpacaLiveDataClientFactory
from nautilus_trader.adapters.alpaca.factories import AlpacaLiveExecClientFactory


__all__ = [
    "ALPACA",
    "ALPACA_CLIENT_ID",
    "ALPACA_VENUE",
    "AlpacaDataClientConfig",
    "AlpacaExecClientConfig",
    "AlpacaLiveDataClientFactory",
    "AlpacaLiveExecClientFactory",
]
