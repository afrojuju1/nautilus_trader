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
Instrument provider for Alpaca US equities, ETFs, and exact option contracts.
"""

import re
from dataclasses import dataclass
from datetime import UTC
from datetime import datetime
from decimal import Decimal

from nautilus_trader.adapters.alpaca.common import normalize_alpaca_symbol
from nautilus_trader.adapters.alpaca.constants import ALPACA_VENUE
from nautilus_trader.common.providers import InstrumentProvider
from nautilus_trader.config import InstrumentProviderConfig
from nautilus_trader.core.datetime import dt_to_unix_nanos
from nautilus_trader.model.currencies import USD
from nautilus_trader.model.enums import AssetClass
from nautilus_trader.model.enums import OptionKind
from nautilus_trader.model.identifiers import InstrumentId
from nautilus_trader.model.identifiers import Symbol
from nautilus_trader.model.instruments import Equity
from nautilus_trader.model.instruments import OptionContract
from nautilus_trader.model.objects import Price
from nautilus_trader.model.objects import Quantity


_OCC_SYMBOL_RE = re.compile(
    r"^(?P<underlying>[A-Z0-9]{1,6})(?P<expiry>\d{6})(?P<kind>[CP])(?P<strike>\d{8})$",
)


def make_alpaca_equity(symbol: str, ts_init: int = 0) -> Equity:
    """
    Create a static Nautilus equity instrument for an Alpaca US equity/ETF symbol.
    """
    normalized = normalize_alpaca_symbol(symbol)
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


def make_alpaca_option(symbol: str, ts_init: int = 0) -> OptionContract:
    """
    Create a static Nautilus option contract for an exact Alpaca/OCC option symbol.
    """
    contract = _parse_occ_option_symbol(symbol)
    return OptionContract(
        instrument_id=InstrumentId(Symbol(contract.symbol), ALPACA_VENUE),
        raw_symbol=Symbol(contract.symbol),
        asset_class=AssetClass.EQUITY,
        currency=USD,
        price_precision=2,
        price_increment=Price.from_str("0.01"),
        multiplier=Quantity.from_int(100),
        lot_size=Quantity.from_int(1),
        underlying=contract.underlying,
        option_kind=contract.option_kind,
        strike_price=Price.from_str(_decimal_to_plain_str(contract.strike_price)),
        activation_ns=0,
        expiration_ns=contract.expiration_ns,
        ts_event=ts_init,
        ts_init=ts_init,
        info={"provider": "alpaca-static-option", "occ_symbol": contract.symbol},
    )


class AlpacaInstrumentProvider(InstrumentProvider):
    """
    Provides static Alpaca instruments for configured equities and exact option contracts.
    """

    def __init__(
        self,
        equity_symbols: list[str] | None = None,
        option_symbols: list[str] | None = None,
        config: InstrumentProviderConfig | None = None,
    ) -> None:
        super().__init__(config=config)
        self._equity_symbols = tuple(
            normalize_alpaca_symbol(symbol) for symbol in equity_symbols or []
        )
        self._option_symbols = tuple(
            _normalize_option_symbol(symbol) for symbol in option_symbols or []
        )

    async def load_all_async(self, filters: dict | None = None) -> None:
        equity_symbols, option_symbols = self._configured_symbols()
        for symbol in equity_symbols:
            self.add(make_alpaca_equity(symbol))
        for symbol in option_symbols:
            self.add(make_alpaca_option(symbol))

    async def load_ids_async(
        self,
        instrument_ids: list[InstrumentId],
        filters: dict | None = None,
    ) -> None:
        for instrument_id in instrument_ids:
            if instrument_id.venue != ALPACA_VENUE:
                self._log.warning(f"Skipping non-Alpaca instrument id {instrument_id}")
                continue

            symbol = instrument_id.symbol.value
            if is_alpaca_option_symbol(symbol):
                self.add(make_alpaca_option(symbol))
            else:
                self.add(make_alpaca_equity(symbol))

    async def load_async(
        self,
        instrument_id: InstrumentId,
        filters: dict | None = None,
    ) -> None:
        await self.load_ids_async([instrument_id], filters)

    def _configured_symbols(self) -> tuple[list[str], list[str]]:
        equity_symbols = set(self._equity_symbols)
        option_symbols = set(self._option_symbols)
        load_ids = self._config.load_ids or frozenset()

        for raw_instrument_id in load_ids:
            instrument_id = (
                raw_instrument_id
                if isinstance(raw_instrument_id, InstrumentId)
                else InstrumentId.from_str(str(raw_instrument_id))
            )
            if instrument_id.venue == ALPACA_VENUE:
                symbol = instrument_id.symbol.value
                if is_alpaca_option_symbol(symbol):
                    option_symbols.add(_normalize_option_symbol(symbol))
                else:
                    equity_symbols.add(normalize_alpaca_symbol(symbol))

        return sorted(equity_symbols), sorted(option_symbols)


class AlpacaEquityInstrumentProvider(AlpacaInstrumentProvider):
    """
    Provides static Alpaca equity instruments for configured US equity/ETF symbols.
    """

    def __init__(
        self,
        symbols: list[str] | None = None,
        config: InstrumentProviderConfig | None = None,
    ) -> None:
        super().__init__(equity_symbols=symbols, config=config)


def is_alpaca_option_symbol(symbol: str) -> bool:
    try:
        _parse_occ_option_symbol(symbol)
    except ValueError:
        return False
    return True


def _normalize_option_symbol(symbol: str) -> str:
    return _parse_occ_option_symbol(symbol).symbol


@dataclass(frozen=True)
class _OccOptionSymbol:
    symbol: str
    underlying: str
    option_kind: OptionKind
    strike_price: Decimal
    expiration_ns: int


def _parse_occ_option_symbol(symbol: str) -> _OccOptionSymbol:
    normalized = normalize_alpaca_symbol(symbol)
    if normalized.endswith(f".{ALPACA_VENUE.value}"):
        normalized = normalized[: -(len(ALPACA_VENUE.value) + 1)]
    normalized = normalized.removeprefix("O:")

    match = _OCC_SYMBOL_RE.fullmatch(normalized)
    if match is None:
        raise ValueError(f"Alpaca option symbol must use OCC format, got {symbol!r}")

    expiry = match.group("expiry")
    year = 2000 + int(expiry[0:2])
    month = int(expiry[2:4])
    day = int(expiry[4:6])
    expiration = datetime(year, month, day, tzinfo=UTC)
    strike_price = Decimal(match.group("strike")) / Decimal(1000)
    option_kind = OptionKind.CALL if match.group("kind") == "C" else OptionKind.PUT
    return _OccOptionSymbol(
        symbol=normalized,
        underlying=match.group("underlying"),
        option_kind=option_kind,
        strike_price=strike_price,
        expiration_ns=dt_to_unix_nanos(expiration),
    )


def _decimal_to_plain_str(value: Decimal) -> str:
    normalized = value.normalize()
    text = format(normalized, "f")
    if "." in text:
        text = text.rstrip("0").rstrip(".")
    return text
