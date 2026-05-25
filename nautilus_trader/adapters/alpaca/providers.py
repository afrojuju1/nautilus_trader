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
Instrument provider for Alpaca US equities and ETFs.
"""

from nautilus_trader.adapters.alpaca.constants import ALPACA_VENUE
from nautilus_trader.common.providers import InstrumentProvider
from nautilus_trader.config import InstrumentProviderConfig
from nautilus_trader.model.currencies import USD
from nautilus_trader.model.identifiers import InstrumentId
from nautilus_trader.model.identifiers import Symbol
from nautilus_trader.model.instruments import Equity
from nautilus_trader.model.objects import Price
from nautilus_trader.model.objects import Quantity


def make_alpaca_equity(symbol: str, ts_init: int = 0) -> Equity:
    """
    Create a static Nautilus equity instrument for an Alpaca US equity/ETF symbol.
    """
    normalized = _normalize_symbol(symbol)
    return Equity(
        instrument_id=InstrumentId(Symbol(normalized), ALPACA_VENUE),
        raw_symbol=Symbol(normalized),
        currency=USD,
        price_precision=2,
        price_increment=Price.from_str("0.01"),
        lot_size=Quantity.from_int(1),
        ts_event=ts_init,
        ts_init=ts_init,
        info={"provider": "alpaca-static-equity"},
    )


class AlpacaEquityInstrumentProvider(InstrumentProvider):
    """
    Provides static Alpaca equity instruments for configured US equity/ETF symbols.
    """

    def __init__(
        self,
        symbols: list[str] | None = None,
        config: InstrumentProviderConfig | None = None,
    ) -> None:
        super().__init__(config=config)
        self._symbols = tuple(_normalize_symbol(symbol) for symbol in symbols or [])

    async def load_all_async(self, filters: dict | None = None) -> None:
        for symbol in self._configured_symbols():
            self.add(make_alpaca_equity(symbol))

    async def load_ids_async(
        self,
        instrument_ids: list[InstrumentId],
        filters: dict | None = None,
    ) -> None:
        for instrument_id in instrument_ids:
            if instrument_id.venue != ALPACA_VENUE:
                self._log.warning(f"Skipping non-Alpaca instrument id {instrument_id}")
                continue

            self.add(make_alpaca_equity(instrument_id.symbol.value))

    async def load_async(
        self,
        instrument_id: InstrumentId,
        filters: dict | None = None,
    ) -> None:
        await self.load_ids_async([instrument_id], filters)

    def _configured_symbols(self) -> list[str]:
        symbols = set(self._symbols)
        load_ids = self._config.load_ids or frozenset()

        for raw_instrument_id in load_ids:
            instrument_id = (
                raw_instrument_id
                if isinstance(raw_instrument_id, InstrumentId)
                else InstrumentId.from_str(str(raw_instrument_id))
            )
            if instrument_id.venue == ALPACA_VENUE:
                symbols.add(_normalize_symbol(instrument_id.symbol.value))

        return sorted(symbols)


def _normalize_symbol(symbol: str) -> str:
    normalized = symbol.strip().upper()
    if not normalized:
        raise ValueError("symbol must not be empty")
    return normalized
