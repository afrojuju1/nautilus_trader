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
Alpaca strategy scaffolds.
"""

from nautilus_trader.adapters.alpaca.strategies.gap_down_fragile_rebound import (
    GapDownFragileRebound,
)
from nautilus_trader.adapters.alpaca.strategies.gap_down_fragile_rebound import (
    GapDownFragileReboundConfig,
)
from nautilus_trader.adapters.alpaca.strategies.put_credit import AlpacaPutCreditStrategy
from nautilus_trader.adapters.alpaca.strategies.put_credit import AlpacaPutCreditStrategyConfig
from nautilus_trader.adapters.alpaca.strategies.upside_gap_continuation import UpsideGapContinuation
from nautilus_trader.adapters.alpaca.strategies.upside_gap_continuation import (
    UpsideGapContinuationConfig,
)


__all__ = [
    "AlpacaPutCreditStrategy",
    "AlpacaPutCreditStrategyConfig",
    "GapDownFragileRebound",
    "GapDownFragileReboundConfig",
    "UpsideGapContinuation",
    "UpsideGapContinuationConfig",
]
