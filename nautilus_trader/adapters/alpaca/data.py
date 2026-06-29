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
Alpaca data client for Python ``TradingNode`` usage.
"""

import asyncio
from datetime import UTC
from datetime import datetime
from datetime import timedelta
from typing import Any

import msgspec
import pandas as pd

from nautilus_trader.adapters.alpaca.common import alpaca_auth_headers
from nautilus_trader.adapters.alpaca.common import format_alpaca_datetime
from nautilus_trader.adapters.alpaca.common import resolve_alpaca_credentials
from nautilus_trader.adapters.alpaca.config import AlpacaDataClientConfig
from nautilus_trader.adapters.alpaca.constants import ALPACA_DATA_BASE_URL
from nautilus_trader.adapters.alpaca.constants import ALPACA_VENUE
from nautilus_trader.adapters.alpaca.providers import AlpacaInstrumentProvider
from nautilus_trader.adapters.alpaca.providers import is_alpaca_option_symbol
from nautilus_trader.adapters.alpaca.snapshots import greeks_from_option_snapshot
from nautilus_trader.adapters.alpaca.snapshots import quote_tick_from_option_snapshot
from nautilus_trader.adapters.alpaca.snapshots import snapshot_for_symbol
from nautilus_trader.cache.cache import Cache
from nautilus_trader.common.component import LiveClock
from nautilus_trader.common.component import MessageBus
from nautilus_trader.common.enums import LogColor
from nautilus_trader.core import nautilus_pyo3
from nautilus_trader.core.datetime import dt_to_unix_nanos
from nautilus_trader.data.messages import RequestBars
from nautilus_trader.data.messages import RequestInstrument
from nautilus_trader.data.messages import RequestInstruments
from nautilus_trader.data.messages import RequestQuoteTicks
from nautilus_trader.data.messages import SubscribeBars
from nautilus_trader.data.messages import SubscribeInstrument
from nautilus_trader.data.messages import SubscribeInstruments
from nautilus_trader.data.messages import SubscribeOptionGreeks
from nautilus_trader.data.messages import SubscribeQuoteTicks
from nautilus_trader.data.messages import UnsubscribeBars
from nautilus_trader.data.messages import UnsubscribeInstrument
from nautilus_trader.data.messages import UnsubscribeInstruments
from nautilus_trader.data.messages import UnsubscribeOptionGreeks
from nautilus_trader.data.messages import UnsubscribeQuoteTicks
from nautilus_trader.live.data_client import LiveMarketDataClient
from nautilus_trader.model.data import Bar
from nautilus_trader.model.data import BarType
from nautilus_trader.model.enums import BarAggregation
from nautilus_trader.model.enums import PriceType
from nautilus_trader.model.identifiers import ClientId
from nautilus_trader.model.identifiers import InstrumentId
from nautilus_trader.model.instruments import Instrument
from nautilus_trader.model.objects import Quantity


ALPACA_STOCK_BARS_PAGE_LIMIT = 10_000


class AlpacaDataClient(LiveMarketDataClient):
    """
    Provides Alpaca instruments, stock bars, and option snapshot quotes/Greeks.
    """

    def __init__(
        self,
        loop: asyncio.AbstractEventLoop,
        msgbus: MessageBus,
        cache: Cache,
        clock: LiveClock,
        instrument_provider: AlpacaInstrumentProvider,
        config: AlpacaDataClientConfig,
        name: str | None = None,
    ) -> None:
        super().__init__(
            loop=loop,
            client_id=ClientId(name or ALPACA_VENUE.value),
            venue=ALPACA_VENUE,
            msgbus=msgbus,
            cache=cache,
            clock=clock,
            instrument_provider=instrument_provider,
            config=config,
        )

        self._config = config
        self._instrument_provider = instrument_provider
        self._http_client = nautilus_pyo3.HttpClient(timeout_secs=config.request_timeout_secs)
        self._data_base_url = (config.data_base_url or ALPACA_DATA_BASE_URL).rstrip("/")
        self._api_key, self._api_secret = resolve_alpaca_credentials(
            config.api_key,
            config.api_secret,
        )
        self._bars_timestamp_on_close = config.bars_timestamp_on_close
        self._bar_poll_interval_secs = config.bar_poll_interval_secs
        self._snapshot_greeks_poll_secs = config.snapshot_greeks_poll_secs
        self._bar_poll_tasks: dict[BarType, asyncio.Task] = {}
        self._last_bar_ts_by_type: dict[BarType, int] = {}
        self._option_snapshot_poll_tasks: dict[InstrumentId, asyncio.Task] = {}

        self._log.info(f"data_base_url={self._data_base_url}", LogColor.BLUE)
        self._log.info(f"stock_feed={config.stock_feed}", LogColor.BLUE)
        self._log.info(
            f"bar_poll_interval_secs={self._bar_poll_interval_secs}",
            LogColor.BLUE,
        )
        self._log.info(
            f"bars_timestamp_on_close={self._bars_timestamp_on_close}",
            LogColor.BLUE,
        )
        self._log.info(f"option_feed={config.option_feed}", LogColor.BLUE)
        self._log.info(
            f"snapshot_greeks_poll_secs={self._snapshot_greeks_poll_secs}",
            LogColor.BLUE,
        )

    async def _connect(self) -> None:
        await self._instrument_provider.load_all_async()

        for instrument in self._instrument_provider.list_all():
            self._handle_data(instrument)

    async def _disconnect(self) -> None:
        for task in self._bar_poll_tasks.values():
            task.cancel()
        self._bar_poll_tasks.clear()
        for task in self._option_snapshot_poll_tasks.values():
            task.cancel()
        self._option_snapshot_poll_tasks.clear()

    async def _subscribe_instruments(self, command: SubscribeInstruments) -> None:
        for instrument in self._instrument_provider.list_all():
            self._handle_data(instrument)

    async def _subscribe_instrument(self, command: SubscribeInstrument) -> None:
        instrument = await self._load_instrument_if_needed(command.instrument_id)
        if instrument is not None:
            self._handle_data(instrument)

    async def _subscribe_bars(self, command: SubscribeBars) -> None:
        bar_type = command.bar_type
        if bar_type in self._bar_poll_tasks:
            return

        task = self.create_task(
            self._poll_bars(bar_type),
            log_msg=f"alpaca_poll_bars_{bar_type}",
        )
        if task is not None:
            self._bar_poll_tasks[bar_type] = task

    async def _unsubscribe_instruments(self, command: UnsubscribeInstruments) -> None:
        return None

    async def _unsubscribe_instrument(self, command: UnsubscribeInstrument) -> None:
        return None

    async def _unsubscribe_bars(self, command: UnsubscribeBars) -> None:
        task = self._bar_poll_tasks.pop(command.bar_type, None)
        if task is not None:
            task.cancel()

    async def _subscribe_quote_ticks(self, command: SubscribeQuoteTicks) -> None:
        if not is_alpaca_option_symbol(command.instrument_id.symbol.value):
            self._log.error(
                "Alpaca quote subscriptions currently require an option symbol: "
                f"{command.instrument_id}",
            )
            return

        instrument = await self._load_instrument_if_needed(command.instrument_id)
        if instrument is None:
            self._log.error(
                f"Cannot subscribe quotes: no Alpaca instrument {command.instrument_id}",
            )
            return

        snapshot = await self._fetch_option_snapshot(command.instrument_id.symbol.value)
        quote = quote_tick_from_option_snapshot(
            instrument=instrument,
            snapshot=snapshot,
            ts_init=self._clock.timestamp_ns(),
        )
        if quote is not None:
            self._handle_data(quote)
        self._ensure_option_snapshot_polling(command.instrument_id)

    async def _unsubscribe_quote_ticks(self, command: UnsubscribeQuoteTicks) -> None:
        self._stop_option_snapshot_polling_if_unused(command.instrument_id)

    async def _subscribe_option_greeks(self, command: SubscribeOptionGreeks) -> None:
        if not is_alpaca_option_symbol(command.instrument_id.symbol.value):
            self._log.error(
                "Alpaca option Greeks subscriptions require an option symbol: "
                f"{command.instrument_id}",
            )
            return

        instrument = await self._load_instrument_if_needed(command.instrument_id)
        if instrument is None:
            self._log.error(
                f"Cannot subscribe option Greeks: no Alpaca instrument {command.instrument_id}",
            )
            return

        snapshot = await self._fetch_option_snapshot(command.instrument_id.symbol.value)
        greeks = greeks_from_option_snapshot(
            instrument_id=command.instrument_id,
            snapshot=snapshot,
            ts_init=self._clock.timestamp_ns(),
        )
        if greeks is not None:
            self._handle_data(greeks)
        self._ensure_option_snapshot_polling(command.instrument_id)

    async def _unsubscribe_option_greeks(self, command: UnsubscribeOptionGreeks) -> None:
        self._stop_option_snapshot_polling_if_unused(command.instrument_id)

    async def _request_instrument(self, request: RequestInstrument) -> None:
        instrument = await self._load_instrument_if_needed(request.instrument_id)
        if instrument is not None:
            self._handle_instrument(
                instrument,
                request.id,
                request.start,
                request.end,
                request.params,
            )

    async def _request_instruments(self, request: RequestInstruments) -> None:
        self._handle_instruments(
            ALPACA_VENUE,
            self._instrument_provider.list_all(),
            request.id,
            request.start,
            request.end,
            request.params,
        )

    async def _request_quote_ticks(self, request: RequestQuoteTicks) -> None:
        instrument = await self._load_instrument_if_needed(request.instrument_id)
        ticks = []
        if instrument is not None and is_alpaca_option_symbol(request.instrument_id.symbol.value):
            try:
                snapshot = await self._fetch_option_snapshot(request.instrument_id.symbol.value)
                quote = quote_tick_from_option_snapshot(
                    instrument=instrument,
                    snapshot=snapshot,
                    ts_init=self._clock.timestamp_ns(),
                )
                if quote is not None:
                    ticks.append(quote)
            except Exception as e:
                self._log.exception(
                    f"Failed to request Alpaca option quote for {request.instrument_id}",
                    e,
                )

        self._handle_quote_ticks(
            request.instrument_id,
            ticks,
            request.id,
            request.start,
            request.end,
            request.params,
        )

    async def _request_bars(self, request: RequestBars) -> None:
        try:
            bars = await self._fetch_stock_bars(
                bar_type=request.bar_type,
                start=request.start,
                end=request.end,
                limit=request.limit if request.limit > 0 else None,
            )
            self._record_latest_bar_ts(request.bar_type, bars)
            self._handle_bars(
                request.bar_type,
                bars,
                request.id,
                request.start,
                request.end,
                request.params,
            )
        except Exception as e:
            self._log.exception(f"Failed to request Alpaca bars for {request.bar_type}", e)
            self._handle_bars(
                request.bar_type,
                [],
                request.id,
                request.start,
                request.end,
                request.params,
            )

    async def _poll_bars(self, bar_type: BarType) -> None:
        while True:
            await asyncio.sleep(self._bar_poll_interval_secs)
            end = datetime.now(UTC)
            start = end - timedelta(days=14)
            try:
                bars = await self._fetch_stock_bars(
                    bar_type=bar_type,
                    start=start,
                    end=end,
                    limit=None,
                )
            except asyncio.CancelledError:
                raise
            except Exception as e:
                self._log.exception(f"Failed polling Alpaca bars for {bar_type}", e)
                continue

            last_ts = self._last_bar_ts_by_type.get(bar_type, 0)
            for bar in bars:
                if bar.ts_event <= last_ts:
                    continue
                self._handle_data(bar)
                last_ts = bar.ts_event
            self._last_bar_ts_by_type[bar_type] = last_ts

    def _ensure_option_snapshot_polling(self, instrument_id: InstrumentId) -> None:
        if self._snapshot_greeks_poll_secs is None:
            return
        if instrument_id in self._option_snapshot_poll_tasks:
            return

        task = self.create_task(
            self._poll_option_snapshot(instrument_id),
            log_msg=f"alpaca_poll_option_snapshot_{instrument_id}",
        )
        if task is not None:
            self._option_snapshot_poll_tasks[instrument_id] = task

    def _stop_option_snapshot_polling_if_unused(self, instrument_id: InstrumentId) -> None:
        if self._has_option_snapshot_subscription(instrument_id):
            return
        task = self._option_snapshot_poll_tasks.pop(instrument_id, None)
        if task is not None:
            task.cancel()

    def _has_option_snapshot_subscription(self, instrument_id: InstrumentId) -> bool:
        return (
            self.is_subscribed_quote_ticks(
                instrument_id,
            )
            or instrument_id in self.subscribed_option_greeks()
        )

    async def _poll_option_snapshot(self, instrument_id: InstrumentId) -> None:
        while True:
            await asyncio.sleep(self._snapshot_greeks_poll_secs or 60)
            if not self._has_option_snapshot_subscription(instrument_id):
                self._stop_option_snapshot_polling_if_unused(instrument_id)
                return

            instrument = await self._load_instrument_if_needed(instrument_id)
            if instrument is None:
                continue

            try:
                snapshot = await self._fetch_option_snapshot(instrument_id.symbol.value)
            except asyncio.CancelledError:
                raise
            except Exception as e:
                self._log.exception(f"Failed polling Alpaca option snapshot for {instrument_id}", e)
                continue

            ts_init = self._clock.timestamp_ns()
            if self.is_subscribed_quote_ticks(instrument_id):
                quote = quote_tick_from_option_snapshot(
                    instrument=instrument,
                    snapshot=snapshot,
                    ts_init=ts_init,
                )
                if quote is not None:
                    self._handle_data(quote)
            if instrument_id in self.subscribed_option_greeks():
                greeks = greeks_from_option_snapshot(
                    instrument_id=instrument_id,
                    snapshot=snapshot,
                    ts_init=ts_init,
                )
                if greeks is not None:
                    self._handle_data(greeks)

    async def _fetch_option_snapshot(self, symbol: str) -> dict[str, Any]:
        normalized = symbol.strip().upper()
        endpoint = f"{self._data_base_url}/v1beta1/options/snapshots"
        params: dict[str, Any] = {
            "symbols": normalized,
            "feed": self._config.option_feed,
            "limit": 1,
        }
        response = await self._http_client.get(
            endpoint,
            params=params,
            headers=self._headers(),
            timeout_secs=self._config.request_timeout_secs,
        )
        if not 200 <= response.status < 300:
            body = response.body.decode("utf-8", errors="replace")
            raise RuntimeError(f"Alpaca option snapshot request failed: {response.status} {body}")

        payload = msgspec.json.decode(response.body)
        snapshot = snapshot_for_symbol(payload, normalized)
        if snapshot is None:
            raise RuntimeError(f"Alpaca option snapshot missing {normalized}")
        return snapshot

    async def _fetch_stock_bars(
        self,
        bar_type: BarType,
        start: datetime | pd.Timestamp | None,
        end: datetime | pd.Timestamp | None,
        limit: int | None,
    ) -> list[Bar]:
        instrument = self._instrument_for_bar_type(bar_type)
        symbol = bar_type.instrument_id.symbol.value
        endpoint = f"{self._data_base_url}/v2/stocks/bars"
        params: dict[str, Any] = {
            "symbols": symbol,
            "timeframe": _timeframe_for_bar_type(bar_type),
            "feed": self._config.stock_feed,
            "adjustment": "raw",
            "sort": "asc",
        }

        if start is not None:
            params["start"] = format_alpaca_datetime(start)
        if end is not None:
            params["end"] = format_alpaca_datetime(end)

        rows: list[dict[str, Any]] = []
        remaining = limit
        page_token: str | None = None

        while True:
            page_params = dict(params)
            page_params["limit"] = min(
                remaining or ALPACA_STOCK_BARS_PAGE_LIMIT, ALPACA_STOCK_BARS_PAGE_LIMIT
            )
            if page_token:
                page_params["page_token"] = page_token

            response = await self._http_client.get(
                endpoint,
                params=page_params,
                headers=self._headers(),
                timeout_secs=self._config.request_timeout_secs,
            )
            if not 200 <= response.status < 300:
                body = response.body.decode("utf-8", errors="replace")
                raise RuntimeError(f"Alpaca stock bars request failed: {response.status} {body}")

            payload = msgspec.json.decode(response.body)
            page_rows = _rows_for_symbol(payload, symbol)
            rows.extend(page_rows)

            if remaining is not None:
                remaining -= len(page_rows)
                if remaining <= 0:
                    rows = rows[:limit]
                    break

            page_token = _next_page_token(payload)
            if not page_token:
                break

        return [
            _bar_from_alpaca_row(
                bar_type=bar_type,
                instrument=instrument,
                row=row,
                ts_init=self._clock.timestamp_ns(),
                timestamp_on_close=self._bars_timestamp_on_close,
            )
            for row in rows
        ]

    def _headers(self) -> dict[str, str]:
        return alpaca_auth_headers(
            self._api_key,
            self._api_secret,
            surface="data",
            config_name="AlpacaDataClientConfig",
        )

    def _instrument_for_bar_type(self, bar_type: BarType) -> Instrument:
        instrument = self._instrument_provider.find(bar_type.instrument_id)
        if instrument is None:
            instrument = self._cache.instrument(bar_type.instrument_id)
        if instrument is None:
            raise RuntimeError(f"No Alpaca instrument loaded for {bar_type.instrument_id}")
        return instrument

    async def _load_instrument_if_needed(self, instrument_id: InstrumentId) -> Instrument | None:
        instrument = self._instrument_provider.find(instrument_id)
        if instrument is None:
            await self._instrument_provider.load_async(instrument_id)
            instrument = self._instrument_provider.find(instrument_id)
        if instrument is None:
            instrument = self._cache.instrument(instrument_id)
        return instrument

    def _record_latest_bar_ts(self, bar_type: BarType, bars: list[Bar]) -> None:
        if bars:
            self._last_bar_ts_by_type[bar_type] = max(bar.ts_event for bar in bars)


def _timeframe_for_bar_type(bar_type: BarType) -> str:
    spec = bar_type.spec
    if spec.price_type != PriceType.LAST:
        raise ValueError(f"Alpaca stock bars only support LAST prices, got {spec.price_type}")

    if spec.aggregation == BarAggregation.MINUTE:
        return f"{spec.step}Min"
    if spec.aggregation == BarAggregation.HOUR:
        return f"{spec.step}Hour"
    if spec.aggregation == BarAggregation.DAY:
        return f"{spec.step}Day"
    if spec.aggregation == BarAggregation.WEEK:
        return f"{spec.step}Week"
    if spec.aggregation == BarAggregation.MONTH:
        return f"{spec.step}Month"

    raise ValueError(f"Unsupported Alpaca stock bar aggregation {spec.aggregation}")


def _rows_for_symbol(payload: Any, symbol: str) -> list[dict[str, Any]]:
    if not isinstance(payload, dict):
        return []

    bars = payload.get("bars", {})
    if isinstance(bars, dict):
        rows = bars.get(symbol) or bars.get(symbol.upper()) or []
    elif isinstance(bars, list):
        rows = bars
    else:
        rows = []

    return [row for row in rows if isinstance(row, dict)]


def _next_page_token(payload: Any) -> str | None:
    if isinstance(payload, dict):
        token = payload.get("next_page_token")
        return token if isinstance(token, str) and token else None
    return None


def _bar_from_alpaca_row(
    bar_type: BarType,
    instrument: Instrument,
    row: dict[str, Any],
    ts_init: int,
    timestamp_on_close: bool,
) -> Bar:
    ts_event = _row_timestamp_ns(row["t"], bar_type, timestamp_on_close)
    return Bar(
        bar_type=bar_type,
        open=instrument.make_price(row["o"]),
        high=instrument.make_price(row["h"]),
        low=instrument.make_price(row["l"]),
        close=instrument.make_price(row["c"]),
        volume=Quantity.from_int(int(row.get("v") or 0)),
        ts_event=ts_event,
        ts_init=ts_init,
    )


def _row_timestamp_ns(value: str, bar_type: BarType, timestamp_on_close: bool) -> int:
    timestamp = pd.Timestamp(value)
    if timestamp.tzinfo is None:
        timestamp = timestamp.tz_localize("UTC")
    else:
        timestamp = timestamp.tz_convert("UTC")

    if timestamp_on_close:
        timestamp += pd.Timedelta(bar_type.spec.get_interval_ns(), unit="ns")

    return dt_to_unix_nanos(timestamp)
