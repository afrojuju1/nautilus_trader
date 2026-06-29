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
Alpaca broker execution client for US equity/ETF orders and option multi-leg orders.
"""

import asyncio
from decimal import Decimal
from typing import Any

import msgspec

from nautilus_trader.adapters.alpaca.common import alpaca_auth_headers
from nautilus_trader.adapters.alpaca.common import format_alpaca_datetime
from nautilus_trader.adapters.alpaca.common import normalize_alpaca_symbol
from nautilus_trader.adapters.alpaca.common import resolve_alpaca_credentials
from nautilus_trader.adapters.alpaca.common import timestamp_ns_from_value
from nautilus_trader.adapters.alpaca.config import AlpacaExecClientConfig
from nautilus_trader.adapters.alpaca.constants import ALPACA_LIVE_TRADING_BASE_URL
from nautilus_trader.adapters.alpaca.constants import ALPACA_PAPER_TRADING_BASE_URL
from nautilus_trader.adapters.alpaca.constants import ALPACA_VENUE
from nautilus_trader.adapters.alpaca.orders import mleg_leg_snapshot_for_order
from nautilus_trader.adapters.alpaca.orders import mleg_order_plan_from_order_list
from nautilus_trader.adapters.alpaca.orders import mleg_payload_from_order_plan
from nautilus_trader.adapters.alpaca.orders import mleg_report_leg_snapshot
from nautilus_trader.adapters.alpaca.orders import nested_order_legs
from nautilus_trader.adapters.alpaca.providers import AlpacaInstrumentProvider
from nautilus_trader.adapters.alpaca.providers import is_alpaca_option_symbol
from nautilus_trader.adapters.alpaca.providers import make_alpaca_equity
from nautilus_trader.adapters.alpaca.providers import make_alpaca_option
from nautilus_trader.cache.cache import Cache
from nautilus_trader.common.component import LiveClock
from nautilus_trader.common.component import MessageBus
from nautilus_trader.common.enums import LogColor
from nautilus_trader.core import nautilus_pyo3
from nautilus_trader.core.uuid import UUID4
from nautilus_trader.execution.messages import BatchCancelOrders
from nautilus_trader.execution.messages import CancelAllOrders
from nautilus_trader.execution.messages import CancelOrder
from nautilus_trader.execution.messages import GenerateFillReports
from nautilus_trader.execution.messages import GenerateOrderStatusReport
from nautilus_trader.execution.messages import GenerateOrderStatusReports
from nautilus_trader.execution.messages import GeneratePositionStatusReports
from nautilus_trader.execution.messages import ModifyOrder
from nautilus_trader.execution.messages import QueryAccount
from nautilus_trader.execution.messages import SubmitOrder
from nautilus_trader.execution.messages import SubmitOrderList
from nautilus_trader.execution.reports import FillReport
from nautilus_trader.execution.reports import OrderStatusReport
from nautilus_trader.execution.reports import PositionStatusReport
from nautilus_trader.live.execution_client import LiveExecutionClient
from nautilus_trader.model.currencies import USD
from nautilus_trader.model.currencies import Currency
from nautilus_trader.model.enums import AccountType
from nautilus_trader.model.enums import LiquiditySide
from nautilus_trader.model.enums import OmsType
from nautilus_trader.model.enums import OrderSide
from nautilus_trader.model.enums import OrderStatus
from nautilus_trader.model.enums import OrderType
from nautilus_trader.model.enums import PositionSide
from nautilus_trader.model.enums import TimeInForce
from nautilus_trader.model.identifiers import AccountId
from nautilus_trader.model.identifiers import ClientId
from nautilus_trader.model.identifiers import ClientOrderId
from nautilus_trader.model.identifiers import InstrumentId
from nautilus_trader.model.identifiers import TradeId
from nautilus_trader.model.identifiers import VenueOrderId
from nautilus_trader.model.instruments import Instrument
from nautilus_trader.model.objects import AccountBalance
from nautilus_trader.model.objects import Money
from nautilus_trader.model.objects import Quantity
from nautilus_trader.model.orders import Order


ALPACA_ORDER_PAGE_LIMIT = 500
ALPACA_ACTIVITY_PAGE_LIMIT = 100

_ORDER_STATUS_BY_ALPACA_STATUS = {
    "accepted": OrderStatus.ACCEPTED,
    "accepted_for_bidding": OrderStatus.ACCEPTED,
    "calculated": OrderStatus.REJECTED,
    "canceled": OrderStatus.CANCELED,
    "done_for_day": OrderStatus.EXPIRED,
    "expired": OrderStatus.EXPIRED,
    "filled": OrderStatus.FILLED,
    "new": OrderStatus.ACCEPTED,
    "partially_filled": OrderStatus.PARTIALLY_FILLED,
    "pending_cancel": OrderStatus.PENDING_CANCEL,
    "pending_new": OrderStatus.SUBMITTED,
    "pending_replace": OrderStatus.PENDING_UPDATE,
    "rejected": OrderStatus.REJECTED,
    "replaced": OrderStatus.PENDING_UPDATE,
    "stopped": OrderStatus.REJECTED,
    "suspended": OrderStatus.REJECTED,
}


class AlpacaExecutionClient(LiveExecutionClient):
    """
    Provides broker execution for Alpaca equity/ETF limits and option multi-leg limits.
    """

    def __init__(
        self,
        loop: asyncio.AbstractEventLoop,
        msgbus: MessageBus,
        cache: Cache,
        clock: LiveClock,
        instrument_provider: AlpacaInstrumentProvider,
        config: AlpacaExecClientConfig,
        name: str | None = None,
    ) -> None:
        client_id = ClientId(name or ALPACA_VENUE.value)
        super().__init__(
            loop=loop,
            client_id=client_id,
            venue=ALPACA_VENUE,
            oms_type=OmsType.NETTING,
            account_type=AccountType.MARGIN,
            base_currency=None,
            instrument_provider=instrument_provider,
            msgbus=msgbus,
            cache=cache,
            clock=clock,
            config=config,
        )
        self._set_account_id(AccountId(f"{client_id}-001"))

        self._config = config
        self._instrument_provider = instrument_provider
        self._http_client = nautilus_pyo3.HttpClient(timeout_secs=config.request_timeout_secs)
        self._trading_base_url = (
            config.trading_base_url
            or (
                ALPACA_LIVE_TRADING_BASE_URL
                if config.environment == "live"
                else ALPACA_PAPER_TRADING_BASE_URL
            )
        ).rstrip("/")
        self._api_key, self._api_secret = resolve_alpaca_credentials(
            config.api_key,
            config.api_secret,
        )
        self._accepted_venue_order_ids: set[VenueOrderId] = set()
        self._terminal_venue_order_ids: set[VenueOrderId] = set()
        self._filled_qty_by_venue_order_id: dict[VenueOrderId, Decimal] = {}
        self._last_buying_power: Decimal | None = None
        self._remote_risk_state_loaded = False
        self._remote_position_qty_by_symbol: dict[str, Decimal] = {}
        self._remote_position_notional_by_symbol: dict[str, Decimal] = {}
        self._remote_open_order_qty_by_symbol_side: dict[tuple[str, OrderSide], Decimal] = {}
        self._remote_open_order_notional_by_symbol: dict[str, Decimal] = {}
        self._remote_open_client_order_ids: set[ClientOrderId] = set()
        self._mleg_parent_venue_order_id_by_client_order_id: dict[
            ClientOrderId,
            VenueOrderId,
        ] = {}
        self._poll_task: asyncio.Task | None = None

        self._log.info(f"environment={config.environment}", LogColor.BLUE)
        self._log.info(f"trading_base_url={self._trading_base_url}", LogColor.BLUE)
        self._log.info(
            f"reconciliation_poll_secs={config.reconciliation_poll_secs}",
            LogColor.BLUE,
        )
        self._log.info(f"risk_kill_switch={config.risk_kill_switch}", LogColor.BLUE)
        self._log.info(f"max_order_notional={config.max_order_notional}", LogColor.BLUE)
        self._log.info(f"max_total_notional={config.max_total_notional}", LogColor.BLUE)
        self._log.info(
            f"allow_duplicate_symbol_exposure={config.allow_duplicate_symbol_exposure}",
            LogColor.BLUE,
        )
        self._log.info(f"allow_short_selling={config.allow_short_selling}", LogColor.BLUE)
        if config.use_trade_updates_stream:
            self._log.warning(
                "Python Alpaca execution uses REST reconciliation; trade update WebSocket "
                "handling remains in the Rust Alpaca runtime.",
            )

    async def _connect(self) -> None:
        await self._instrument_provider.load_all_async()
        await self._update_account_state()
        await self._sync_remote_risk_state()
        await self._sync_open_cached_orders()

        if self._config.reconciliation_poll_secs:
            self._poll_task = self.create_task(
                self._poll_reconciliation(),
                log_msg="alpaca_execution_reconciliation_poll",
            )

    async def _disconnect(self) -> None:
        task = self._poll_task
        if task is None:
            return

        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass
        finally:
            self._poll_task = None

    async def _submit_order(self, command: SubmitOrder) -> None:
        order = command.order
        error = _validate_equity_limit_order(order)
        if error is not None:
            self.generate_order_denied(
                strategy_id=order.strategy_id,
                instrument_id=order.instrument_id,
                client_order_id=order.client_order_id,
                reason=error,
                ts_event=self._clock.timestamp_ns(),
            )
            return

        try:
            await self._sync_remote_risk_state()
        except Exception as e:
            self.generate_order_denied(
                strategy_id=order.strategy_id,
                instrument_id=order.instrument_id,
                client_order_id=order.client_order_id,
                reason=f"RISK_STATE_UNAVAILABLE: {e}",
                ts_event=self._clock.timestamp_ns(),
            )
            return

        risk_reason = self._risk_denial_reason(order)
        if risk_reason is not None:
            self.generate_order_denied(
                strategy_id=order.strategy_id,
                instrument_id=order.instrument_id,
                client_order_id=order.client_order_id,
                reason=risk_reason,
                ts_event=self._clock.timestamp_ns(),
            )
            return

        payload = _equity_limit_payload_from_order(order)
        self.generate_order_submitted(
            strategy_id=order.strategy_id,
            instrument_id=order.instrument_id,
            client_order_id=order.client_order_id,
            ts_event=self._clock.timestamp_ns(),
        )

        try:
            submitted = await self._post_json("/v2/orders", payload)
        except Exception as e:
            self.generate_order_rejected(
                strategy_id=order.strategy_id,
                instrument_id=order.instrument_id,
                client_order_id=order.client_order_id,
                reason=f"submit-order-rejected: {e}",
                ts_event=self._clock.timestamp_ns(),
            )
            return

        self._emit_order_snapshot(order, submitted)

    async def _submit_order_list(self, command: SubmitOrderList) -> None:
        orders = list(command.order_list.orders)
        try:
            order_plan = mleg_order_plan_from_order_list(command)
        except ValueError as e:
            self._deny_orders(orders, str(e))
            return

        try:
            await self._sync_remote_risk_state()
        except Exception as e:
            self._deny_orders(orders, f"RISK_STATE_UNAVAILABLE: {e}")
            return

        risk_reason = self._mleg_risk_denial_reason(orders)
        if risk_reason is not None:
            self._deny_orders(orders, risk_reason)
            return

        payload = mleg_payload_from_order_plan(command.order_list.id, order_plan)
        for order in orders:
            self.generate_order_submitted(
                strategy_id=order.strategy_id,
                instrument_id=order.instrument_id,
                client_order_id=order.client_order_id,
                ts_event=self._clock.timestamp_ns(),
            )

        try:
            submitted = await self._submit_mleg_payload(payload)
        except Exception as e:
            self._reject_orders(orders, f"submit-order-list-rejected: {e}")
            return

        if not isinstance(submitted, dict):
            self._reject_orders(orders, "Alpaca returned no order object")
            return

        self._emit_mleg_order_snapshots(orders, submitted)

    def _deny_orders(self, orders: list[Order], reason: str) -> None:
        for order in orders:
            self.generate_order_denied(
                strategy_id=order.strategy_id,
                instrument_id=order.instrument_id,
                client_order_id=order.client_order_id,
                reason=reason,
                ts_event=self._clock.timestamp_ns(),
            )

    def _reject_orders(self, orders: list[Order], reason: str) -> None:
        for order in orders:
            self.generate_order_rejected(
                strategy_id=order.strategy_id,
                instrument_id=order.instrument_id,
                client_order_id=order.client_order_id,
                reason=reason,
                ts_event=self._clock.timestamp_ns(),
            )

    async def _submit_mleg_payload(self, payload: dict[str, Any]) -> Any:
        submitted = await self._post_json("/v2/orders", payload)
        parent_venue_order_id = _venue_order_id(submitted) if isinstance(submitted, dict) else None
        if parent_venue_order_id is None or nested_order_legs(submitted):
            return submitted

        try:
            fetched = await self._get_json(
                f"/v2/orders/{parent_venue_order_id}",
                {"nested": "true"},
            )
        except Exception as e:
            self._log.warning(f"Failed to fetch nested Alpaca MLeg order: {e}")
            return submitted
        return fetched if isinstance(fetched, dict) else submitted

    async def _modify_order(self, command: ModifyOrder) -> None:
        self.generate_order_modify_rejected(
            strategy_id=command.strategy_id,
            instrument_id=command.instrument_id,
            client_order_id=command.client_order_id,
            venue_order_id=command.venue_order_id,
            reason="UNSUPPORTED_MODIFY_ORDER",
            ts_event=self._clock.timestamp_ns(),
        )

    async def _cancel_order(self, command: CancelOrder) -> None:
        venue_order_id = (
            self._mleg_parent_venue_order_id_by_client_order_id.get(
                command.client_order_id,
            )
            or command.venue_order_id
            or self._cache.venue_order_id(command.client_order_id)
        )
        if venue_order_id is None:
            order = await self._order_by_client_order_id(command.client_order_id)
            venue_order_id = _venue_order_id(order) if order else None

        if venue_order_id is None:
            self.generate_order_cancel_rejected(
                strategy_id=command.strategy_id,
                instrument_id=command.instrument_id,
                client_order_id=command.client_order_id,
                venue_order_id=VenueOrderId("UNKNOWN"),
                reason="UNKNOWN_VENUE_ORDER_ID",
                ts_event=self._clock.timestamp_ns(),
            )
            return

        try:
            await self._delete(f"/v2/orders/{venue_order_id}")
        except Exception as e:
            self.generate_order_cancel_rejected(
                strategy_id=command.strategy_id,
                instrument_id=command.instrument_id,
                client_order_id=command.client_order_id,
                venue_order_id=venue_order_id,
                reason=f"cancel-order-rejected: {e}",
                ts_event=self._clock.timestamp_ns(),
            )
            return

        canceled_orders = self._cached_mleg_orders_for_parent(venue_order_id)
        if canceled_orders:
            for order in canceled_orders:
                self._emit_order_canceled(order=order, parent_venue_order_id=venue_order_id)
            return

        self.generate_order_canceled(
            strategy_id=command.strategy_id,
            instrument_id=command.instrument_id,
            client_order_id=command.client_order_id,
            venue_order_id=venue_order_id,
            ts_event=self._clock.timestamp_ns(),
        )
        self._terminal_venue_order_ids.add(venue_order_id)

    async def _cancel_all_orders(self, command: CancelAllOrders) -> None:
        try:
            await self._delete("/v2/orders")
        except Exception as e:
            self._log.error(f"Failed to cancel all Alpaca orders: {e}")
            return

        for order in self._cache.orders_open(
            venue=ALPACA_VENUE,
            instrument_id=command.instrument_id,
            side=command.order_side,
            account_id=self.account_id,
        ):
            venue_order_id = order.venue_order_id
            if venue_order_id is None:
                continue
            self.generate_order_canceled(
                strategy_id=order.strategy_id,
                instrument_id=order.instrument_id,
                client_order_id=order.client_order_id,
                venue_order_id=venue_order_id,
                ts_event=self._clock.timestamp_ns(),
            )
            self._terminal_venue_order_ids.add(venue_order_id)

    async def _batch_cancel_orders(self, command: BatchCancelOrders) -> None:
        for cancel in command.cancels:
            await self._cancel_order(cancel)

    def _cached_mleg_orders_for_parent(self, parent_venue_order_id: VenueOrderId) -> list[Order]:
        return [
            order
            for order in self._cache.orders_open(venue=ALPACA_VENUE, account_id=self.account_id)
            if self._mleg_parent_venue_order_id_by_client_order_id.get(order.client_order_id)
            == parent_venue_order_id
        ]

    def _emit_order_canceled(self, order: Order, parent_venue_order_id: VenueOrderId) -> None:
        venue_order_id = order.venue_order_id or parent_venue_order_id
        self.generate_order_canceled(
            strategy_id=order.strategy_id,
            instrument_id=order.instrument_id,
            client_order_id=order.client_order_id,
            venue_order_id=venue_order_id,
            ts_event=self._clock.timestamp_ns(),
        )
        self._terminal_venue_order_ids.add(parent_venue_order_id)
        self._terminal_venue_order_ids.add(venue_order_id)

    async def _query_account(self, command: QueryAccount) -> None:
        await self._update_account_state()

    async def generate_order_status_report(
        self,
        command: GenerateOrderStatusReport,
    ) -> OrderStatusReport | None:
        if command.venue_order_id is not None:
            order = await self._get_json(f"/v2/orders/{command.venue_order_id}", {"nested": "true"})
        elif command.client_order_id is not None:
            order = await self._order_snapshot_by_client_order_id(command.client_order_id)
        else:
            raise ValueError("Either venue_order_id or client_order_id must be provided")

        if order is None:
            return None
        return self._order_status_report(order, command.ts_init)

    async def generate_order_status_reports(
        self,
        command: GenerateOrderStatusReports,
    ) -> list[OrderStatusReport]:
        params: dict[str, Any] = {
            "status": "open" if command.open_only else "all",
            "limit": ALPACA_ORDER_PAGE_LIMIT,
            "nested": "true",
            "direction": "desc",
        }
        if command.instrument_id is not None:
            params["symbols"] = command.instrument_id.symbol.value
        if command.start is not None:
            params["after"] = format_alpaca_datetime(command.start)
        if command.end is not None:
            params["until"] = format_alpaca_datetime(command.end)

        orders = await self._get_json("/v2/orders", params)
        if not isinstance(orders, list):
            return []

        reports: list[OrderStatusReport] = []
        for order in orders:
            if not isinstance(order, dict):
                continue
            if self._config.external_order_filtering and not _matches_client_order_id_prefix(
                order,
                self._config.client_order_id_prefix,
            ):
                continue
            nested_legs = nested_order_legs(order)
            if nested_legs:
                for leg in nested_legs:
                    reports.append(
                        self._order_status_report(
                            mleg_report_leg_snapshot(order, leg),
                            command.ts_init,
                        ),
                    )
            else:
                reports.append(self._order_status_report(order, command.ts_init))
        return reports

    async def generate_fill_reports(self, command: GenerateFillReports) -> list[FillReport]:
        params: dict[str, Any] = {
            "page_size": ALPACA_ACTIVITY_PAGE_LIMIT,
            "direction": "desc",
        }
        if command.start is not None:
            params["after"] = format_alpaca_datetime(command.start)
        if command.end is not None:
            params["until"] = format_alpaca_datetime(command.end)

        activities = await self._get_json("/v2/account/activities/FILL", params)
        if not isinstance(activities, list):
            return []

        reports: list[FillReport] = []
        for activity in activities:
            if not isinstance(activity, dict):
                continue
            if command.venue_order_id is not None and activity.get("order_id") != str(
                command.venue_order_id,
            ):
                continue
            report = self._fill_report(activity, command.ts_init)
            if command.instrument_id is not None and report.instrument_id != command.instrument_id:
                continue
            reports.append(report)
        return reports

    async def generate_position_status_reports(
        self,
        command: GeneratePositionStatusReports,
    ) -> list[PositionStatusReport]:
        positions = await self._get_json("/v2/positions")
        if not isinstance(positions, list):
            return []

        reports: list[PositionStatusReport] = []
        for position in positions:
            if not isinstance(position, dict):
                continue
            report = self._position_status_report(position, command.ts_init)
            if command.instrument_id is not None and report.instrument_id != command.instrument_id:
                continue
            reports.append(report)
        return reports

    async def _poll_reconciliation(self) -> None:
        interval = self._config.reconciliation_poll_secs or 60
        while True:
            await asyncio.sleep(interval)
            try:
                await self._update_account_state()
                await self._sync_remote_risk_state()
                await self._sync_open_cached_orders()
            except asyncio.CancelledError:
                raise
            except Exception as e:
                self._log.exception("Alpaca REST reconciliation failed", e)

    async def _update_account_state(self) -> None:
        account = await self._get_json("/v2/account")
        if not isinstance(account, dict):
            raise RuntimeError("Alpaca account response was not an object")
        balance = _account_balance_from_alpaca(account)
        self._last_buying_power = balance.free.as_decimal()
        self.generate_account_state(
            balances=[balance],
            margins=[],
            reported=True,
            ts_event=self._clock.timestamp_ns(),
            info={
                "environment": self._config.environment,
                "account_number": account.get("account_number"),
                "status": account.get("status"),
            },
        )

    async def _sync_remote_risk_state(self) -> None:
        positions = await self._get_json("/v2/positions")
        if not isinstance(positions, list):
            positions = []
        position_qty_by_symbol, position_notional_by_symbol = _equity_position_risk_maps(
            positions,
        )

        open_orders = await self._get_json(
            "/v2/orders",
            {
                "status": "open",
                "limit": ALPACA_ORDER_PAGE_LIMIT,
                "direction": "desc",
            },
        )
        if not isinstance(open_orders, list):
            open_orders = []
        (
            order_qty_by_symbol_side,
            order_notional_by_symbol,
            open_client_order_ids,
        ) = _equity_open_order_risk_maps(open_orders)

        self._remote_position_qty_by_symbol = position_qty_by_symbol
        self._remote_position_notional_by_symbol = position_notional_by_symbol
        self._remote_open_order_qty_by_symbol_side = order_qty_by_symbol_side
        self._remote_open_order_notional_by_symbol = order_notional_by_symbol
        self._remote_open_client_order_ids = open_client_order_ids
        self._remote_risk_state_loaded = True

    async def _sync_open_cached_orders(self) -> None:
        for order in self._cache.orders_open(venue=ALPACA_VENUE, account_id=self.account_id):
            remote = await self._order_snapshot_for_cached_order(order)
            if remote is None:
                continue
            self._emit_order_snapshot(order, remote)

    def _risk_denial_reason(self, order: Order) -> str | None:
        symbol = normalize_alpaca_symbol(order.instrument_id.symbol.value)
        return _equity_risk_denial_reason(
            order=order,
            config=self._config,
            order_notional=_order_notional(order),
            current_total_notional=self._current_total_notional(order.client_order_id),
            current_symbol_position_qty=self._current_symbol_position_qty(symbol),
            same_symbol_exposure_exists=self._same_symbol_exposure_exists(
                symbol,
                order.client_order_id,
            ),
            same_symbol_open_buy_qty=self._same_symbol_open_order_qty(
                symbol,
                OrderSide.BUY,
                order.client_order_id,
            ),
            same_symbol_open_sell_qty=self._same_symbol_open_order_qty(
                symbol,
                OrderSide.SELL,
                order.client_order_id,
            ),
            available_buying_power=self._last_buying_power,
        )

    def _mleg_risk_denial_reason(self, orders: list[Order]) -> str | None:
        if self._config.risk_kill_switch:
            return "RISK_KILL_SWITCH"

        duplicate_ids = [
            str(order.client_order_id)
            for order in orders
            if order.client_order_id in self._remote_open_client_order_ids
        ]
        if duplicate_ids:
            return f"RISK_DUPLICATE_CLIENT_ORDER_ID: {','.join(duplicate_ids)}"
        return None

    async def _order_snapshot_for_cached_order(self, order: Order) -> dict[str, Any] | None:
        remote = await self._order_by_client_order_id(order.client_order_id)
        if remote is not None:
            return remote

        parent_venue_order_id = self._mleg_parent_venue_order_id_by_client_order_id.get(
            order.client_order_id,
        )
        if parent_venue_order_id is None:
            return None

        parent = await self._get_json(f"/v2/orders/{parent_venue_order_id}", {"nested": "true"})
        if not isinstance(parent, dict):
            return None
        return mleg_leg_snapshot_for_order(order, parent)

    async def _order_snapshot_by_client_order_id(
        self,
        client_order_id: ClientOrderId,
    ) -> dict[str, Any] | None:
        remote = await self._order_by_client_order_id(client_order_id)
        if remote is not None:
            return remote

        parent_venue_order_id = self._mleg_parent_venue_order_id_by_client_order_id.get(
            client_order_id,
        )
        if parent_venue_order_id is None:
            return None

        parent = await self._get_json(f"/v2/orders/{parent_venue_order_id}", {"nested": "true"})
        if not isinstance(parent, dict):
            return None
        for order in self._cache.orders_open(venue=ALPACA_VENUE, account_id=self.account_id):
            if order.client_order_id == client_order_id:
                return mleg_leg_snapshot_for_order(order, parent)
        return None

    def _current_total_notional(
        self,
        exclude_client_order_id: ClientOrderId | None = None,
    ) -> Decimal:
        if self._remote_risk_state_loaded:
            total = sum(self._remote_position_notional_by_symbol.values(), Decimal(0)) + sum(
                self._remote_open_order_notional_by_symbol.values(),
                Decimal(0),
            )
        else:
            total = Decimal(0)
            for position in self._cache.positions_open(
                venue=ALPACA_VENUE,
                account_id=self.account_id,
            ):
                total += _position_notional(position)
        for open_order in self._cache.orders_open(venue=ALPACA_VENUE, account_id=self.account_id):
            if (
                exclude_client_order_id is not None
                and open_order.client_order_id == exclude_client_order_id
            ):
                continue
            if open_order.client_order_id in self._remote_open_client_order_ids:
                continue
            total += _order_notional(open_order)
        return total

    def _current_symbol_position_qty(self, symbol: str) -> Decimal:
        if self._remote_risk_state_loaded:
            return self._remote_position_qty_by_symbol.get(symbol, Decimal(0))

        quantity = Decimal(0)
        for position in self._cache.positions_open(venue=ALPACA_VENUE, account_id=self.account_id):
            if normalize_alpaca_symbol(position.instrument_id.symbol.value) == symbol:
                quantity += _signed_position_qty(position)
        return quantity

    def _same_symbol_exposure_exists(
        self,
        symbol: str,
        exclude_client_order_id: ClientOrderId | None = None,
    ) -> bool:
        if self._current_symbol_position_qty(symbol) != 0:
            return True
        return (
            self._same_symbol_open_order_qty(symbol, OrderSide.BUY, exclude_client_order_id) > 0
            or self._same_symbol_open_order_qty(symbol, OrderSide.SELL, exclude_client_order_id) > 0
        )

    def _same_symbol_open_order_qty(
        self,
        symbol: str,
        side: OrderSide,
        exclude_client_order_id: ClientOrderId | None = None,
    ) -> Decimal:
        quantity = Decimal(0)
        if self._remote_risk_state_loaded:
            quantity += self._remote_open_order_qty_by_symbol_side.get((symbol, side), Decimal(0))

        for open_order in self._cache.orders_open(venue=ALPACA_VENUE, account_id=self.account_id):
            if (
                exclude_client_order_id is not None
                and open_order.client_order_id == exclude_client_order_id
            ):
                continue
            if open_order.client_order_id in self._remote_open_client_order_ids:
                continue
            if normalize_alpaca_symbol(open_order.instrument_id.symbol.value) != symbol:
                continue
            if open_order.side == side:
                quantity += _order_qty(open_order)
        return quantity

    async def _order_by_client_order_id(
        self,
        client_order_id: ClientOrderId,
    ) -> dict[str, Any] | None:
        try:
            order = await self._get_json(
                "/v2/orders:by_client_order_id",
                {
                    "client_order_id": str(client_order_id),
                    "nested": "true",
                },
            )
        except RuntimeError as e:
            if "404" in str(e):
                return None
            raise

        return order if isinstance(order, dict) else None

    def _emit_order_snapshot(self, order: Order, data: dict[str, Any]) -> None:
        venue_order_id = _venue_order_id(data)
        ts_event = _timestamp_ns_from_order(data, self._clock.timestamp_ns())
        if venue_order_id is None:
            self.generate_order_rejected(
                strategy_id=order.strategy_id,
                instrument_id=order.instrument_id,
                client_order_id=order.client_order_id,
                reason="Alpaca returned no venue order id",
                ts_event=ts_event,
            )
            return

        status = _order_status_from_alpaca(data.get("status"))
        if status == OrderStatus.REJECTED:
            if venue_order_id not in self._terminal_venue_order_ids:
                self.generate_order_rejected(
                    strategy_id=order.strategy_id,
                    instrument_id=order.instrument_id,
                    client_order_id=order.client_order_id,
                    reason=_alpaca_rejected_reason(data),
                    ts_event=ts_event,
                )
                self._terminal_venue_order_ids.add(venue_order_id)
            return

        if venue_order_id not in self._accepted_venue_order_ids:
            self.generate_order_accepted(
                strategy_id=order.strategy_id,
                instrument_id=order.instrument_id,
                client_order_id=order.client_order_id,
                venue_order_id=venue_order_id,
                ts_event=ts_event,
            )
            self._accepted_venue_order_ids.add(venue_order_id)

        self._emit_fill_delta(order, data, venue_order_id)

        if status == OrderStatus.CANCELED and venue_order_id not in self._terminal_venue_order_ids:
            self.generate_order_canceled(
                strategy_id=order.strategy_id,
                instrument_id=order.instrument_id,
                client_order_id=order.client_order_id,
                venue_order_id=venue_order_id,
                ts_event=ts_event,
            )
            self._terminal_venue_order_ids.add(venue_order_id)
        elif status == OrderStatus.FILLED:
            self._terminal_venue_order_ids.add(venue_order_id)
        elif status == OrderStatus.EXPIRED and venue_order_id not in self._terminal_venue_order_ids:
            self.generate_order_expired(
                strategy_id=order.strategy_id,
                instrument_id=order.instrument_id,
                client_order_id=order.client_order_id,
                venue_order_id=venue_order_id,
                ts_event=ts_event,
            )
            self._terminal_venue_order_ids.add(venue_order_id)

    def _emit_mleg_order_snapshots(self, orders: list[Order], parent: dict[str, Any]) -> None:
        parent_venue_order_id = _venue_order_id(parent)
        if parent_venue_order_id is not None:
            for order in orders:
                self._mleg_parent_venue_order_id_by_client_order_id[order.client_order_id] = (
                    parent_venue_order_id
                )

        for order in orders:
            self._emit_order_snapshot(order, mleg_leg_snapshot_for_order(order, parent))

    def _emit_fill_delta(
        self,
        order: Order,
        data: dict[str, Any],
        venue_order_id: VenueOrderId,
    ) -> None:
        filled_qty = _decimal_from_optional(data.get("filled_qty")) or Decimal(0)
        prior_filled_qty = self._filled_qty_by_venue_order_id.get(venue_order_id, Decimal(0))
        delta_qty = filled_qty - prior_filled_qty
        if delta_qty <= 0:
            return

        instrument = self._instrument_for_symbol(
            data.get("symbol") or order.instrument_id.symbol.value
        )
        fill_price = _decimal_from_optional(data.get("filled_avg_price")) or Decimal(
            str(order.price)
        )
        trade_id = TradeId(f"{venue_order_id}-{format(filled_qty.normalize(), 'f')}")
        self.generate_order_filled(
            strategy_id=order.strategy_id,
            instrument_id=order.instrument_id,
            client_order_id=order.client_order_id,
            venue_order_id=venue_order_id,
            venue_position_id=None,
            trade_id=trade_id,
            order_side=order.side,
            order_type=order.order_type,
            last_qty=instrument.make_qty(delta_qty),
            last_px=instrument.make_price(fill_price),
            quote_currency=instrument.quote_currency,
            commission=Money(0, instrument.quote_currency),
            liquidity_side=LiquiditySide.NO_LIQUIDITY_SIDE,
            ts_event=_timestamp_ns_from_order(data, self._clock.timestamp_ns()),
            info={"source": "alpaca_order_snapshot"},
        )
        self._filled_qty_by_venue_order_id[venue_order_id] = filled_qty

    def _order_status_report(self, data: dict[str, Any], ts_init: int) -> OrderStatusReport:
        symbol = _required_str(data, "symbol")
        instrument = self._instrument_for_symbol(symbol)
        quantity = _quantity_from_decimal(instrument, _required_decimal(data, "qty"))
        filled_qty = _quantity_from_decimal(
            instrument,
            _decimal_from_optional(data.get("filled_qty")) or Decimal(0),
        )
        ts_last = _timestamp_ns_from_order(data, ts_init)
        return OrderStatusReport(
            account_id=self.account_id,
            instrument_id=instrument.id,
            client_order_id=_client_order_id(data),
            venue_order_id=VenueOrderId(_required_str(data, "id")),
            order_side=_order_side_from_alpaca(_required_str(data, "side")),
            order_type=_order_type_from_alpaca(_required_str(data, "type")),
            time_in_force=_time_in_force_from_alpaca(_required_str(data, "time_in_force")),
            order_status=_order_status_from_alpaca(data.get("status")),
            quantity=quantity,
            filled_qty=filled_qty,
            price=(
                instrument.make_price(_required_decimal(data, "limit_price"))
                if data.get("limit_price")
                else None
            ),
            avg_px=_decimal_from_optional(data.get("filled_avg_price")),
            report_id=UUID4(),
            ts_accepted=timestamp_ns_from_value(
                data.get("submitted_at") or data.get("created_at"),
                ts_last,
            ),
            ts_last=ts_last,
            ts_init=ts_init,
        )

    def _fill_report(self, data: dict[str, Any], ts_init: int) -> FillReport:
        symbol = _required_str(data, "symbol")
        instrument = self._instrument_for_symbol(symbol)
        venue_order_id = VenueOrderId(_required_str(data, "order_id"))
        trade_id = TradeId(_required_str(data, "id"))
        ts_event = timestamp_ns_from_value(
            data.get("transaction_time") or data.get("date"),
            ts_init,
        )
        return FillReport(
            account_id=self.account_id,
            instrument_id=instrument.id,
            venue_order_id=venue_order_id,
            client_order_id=self._cache.client_order_id(venue_order_id),
            trade_id=trade_id,
            order_side=_order_side_from_alpaca(_required_str(data, "side")),
            last_qty=_quantity_from_decimal(instrument, _required_decimal(data, "qty")),
            last_px=instrument.make_price(_required_decimal(data, "price")),
            commission=Money(0, instrument.quote_currency),
            liquidity_side=LiquiditySide.NO_LIQUIDITY_SIDE,
            report_id=UUID4(),
            ts_event=ts_event,
            ts_init=ts_init,
        )

    def _position_status_report(self, data: dict[str, Any], ts_init: int) -> PositionStatusReport:
        symbol = _required_str(data, "symbol")
        instrument = self._instrument_for_symbol(symbol)
        qty = _required_decimal(data, "qty")
        return PositionStatusReport(
            account_id=self.account_id,
            instrument_id=instrument.id,
            position_side=_position_side_from_alpaca(data.get("side"), qty),
            quantity=_quantity_from_decimal(instrument, abs(qty)),
            avg_px_open=_decimal_from_optional(data.get("avg_entry_price")),
            report_id=UUID4(),
            ts_last=ts_init,
            ts_init=ts_init,
        )

    def _instrument_for_symbol(self, symbol: str) -> Instrument:
        instrument_id = InstrumentId.from_str(f"{symbol.upper()}.{ALPACA_VENUE}")
        instrument = self._cache.instrument(instrument_id) or self._instrument_provider.find(
            instrument_id,
        )
        if instrument is not None:
            return instrument
        if is_alpaca_option_symbol(symbol):
            return make_alpaca_option(symbol, ts_init=self._clock.timestamp_ns())
        return make_alpaca_equity(symbol, ts_init=self._clock.timestamp_ns())

    async def _get_json(self, path: str, params: dict[str, Any] | None = None) -> Any:
        response = await self._http_client.get(
            f"{self._trading_base_url}{path}",
            params=params,
            headers=self._headers(),
            timeout_secs=self._config.request_timeout_secs,
        )
        return _decode_response(response.status, response.body)

    async def _post_json(self, path: str, payload: dict[str, Any]) -> Any:
        response = await self._http_client.post(
            f"{self._trading_base_url}{path}",
            headers={**self._headers(), "Content-Type": "application/json"},
            body=msgspec.json.encode(payload),
            timeout_secs=self._config.request_timeout_secs,
        )
        return _decode_response(response.status, response.body)

    async def _delete(self, path: str) -> Any:
        response = await self._http_client.delete(
            f"{self._trading_base_url}{path}",
            headers=self._headers(),
            timeout_secs=self._config.request_timeout_secs,
        )
        if response.status == 204:
            return None
        return _decode_response(response.status, response.body)

    def _headers(self) -> dict[str, str]:
        return alpaca_auth_headers(
            self._api_key,
            self._api_secret,
            surface="execution",
            config_name="AlpacaExecClientConfig",
        )


def _validate_equity_limit_order(order: Order) -> str | None:
    if order.instrument_id.venue != ALPACA_VENUE:
        return f"UNSUPPORTED_VENUE: {order.instrument_id.venue}"
    if is_alpaca_option_symbol(order.instrument_id.symbol.value):
        return "UNSUPPORTED_SINGLE_OPTION_ORDER"
    if order.order_type != OrderType.LIMIT:
        return f"UNSUPPORTED_ORDER_TYPE: {order.order_type.name}"
    if order.time_in_force != TimeInForce.DAY:
        return f"UNSUPPORTED_TIME_IN_FORCE: {order.time_in_force.name}"
    if order.side not in (OrderSide.BUY, OrderSide.SELL):
        return f"UNSUPPORTED_ORDER_SIDE: {order.side.name}"
    if order.is_quote_quantity:
        return "UNSUPPORTED_QUOTE_QUANTITY"
    quantity = Decimal(str(order.quantity))
    if quantity <= 0 or quantity != quantity.to_integral_value():
        return "UNSUPPORTED_FRACTIONAL_SHARE_QUANTITY"
    if order.price is None or Decimal(str(order.price)) <= 0:
        return "INVALID_LIMIT_PRICE"
    return None


def _equity_risk_denial_reason(
    *,
    order: Order,
    config: AlpacaExecClientConfig,
    order_notional: Decimal,
    current_total_notional: Decimal,
    current_symbol_position_qty: Decimal,
    same_symbol_exposure_exists: bool,
    same_symbol_open_buy_qty: Decimal,
    same_symbol_open_sell_qty: Decimal,
    available_buying_power: Decimal | None,
) -> str | None:
    if config.risk_kill_switch:
        return "RISK_KILL_SWITCH"

    quantity = _order_qty(order)
    signed_order_qty = quantity if order.side == OrderSide.BUY else -quantity
    short_sale_reason = _short_sale_denial_reason(
        order=order,
        config=config,
        quantity=quantity,
        current_symbol_position_qty=current_symbol_position_qty,
        same_symbol_open_sell_qty=same_symbol_open_sell_qty,
    )
    if short_sale_reason is not None:
        return short_sale_reason

    if order.side == OrderSide.BUY and current_symbol_position_qty < 0:
        closeable_short_qty = abs(current_symbol_position_qty) - same_symbol_open_buy_qty
        if quantity <= max(Decimal(0), closeable_short_qty):
            return None

    projected_symbol_qty = current_symbol_position_qty + signed_order_qty
    risk_increasing = abs(projected_symbol_qty) > abs(current_symbol_position_qty)
    if not risk_increasing:
        return None

    if config.max_order_notional is not None and order_notional > config.max_order_notional:
        return (
            "RISK_MAX_ORDER_NOTIONAL: "
            f"order_notional={order_notional} max_order_notional={config.max_order_notional}"
        )

    if not config.allow_duplicate_symbol_exposure and same_symbol_exposure_exists:
        return f"RISK_DUPLICATE_SYMBOL_EXPOSURE: {order.instrument_id.symbol.value}"

    projected_total_notional = current_total_notional + order_notional
    if (
        config.max_total_notional is not None
        and projected_total_notional > config.max_total_notional
    ):
        return (
            "RISK_MAX_TOTAL_NOTIONAL: "
            f"projected_total_notional={projected_total_notional} "
            f"max_total_notional={config.max_total_notional}"
        )

    return _buying_power_denial_reason(
        config=config,
        order_notional=order_notional,
        available_buying_power=available_buying_power,
    )


def _short_sale_denial_reason(
    *,
    order: Order,
    config: AlpacaExecClientConfig,
    quantity: Decimal,
    current_symbol_position_qty: Decimal,
    same_symbol_open_sell_qty: Decimal,
) -> str | None:
    if config.allow_short_selling or order.side != OrderSide.SELL:
        return None

    closeable_long_qty = max(Decimal(0), current_symbol_position_qty - same_symbol_open_sell_qty)
    if quantity <= closeable_long_qty:
        return None

    return (
        f"RISK_SHORT_SELLING_DISABLED: sell_qty={quantity} closeable_long_qty={closeable_long_qty}"
    )


def _buying_power_denial_reason(
    *,
    config: AlpacaExecClientConfig,
    order_notional: Decimal,
    available_buying_power: Decimal | None,
) -> str | None:
    if available_buying_power is None or not config.enforce_buying_power:
        return None

    if config.max_buying_power_pct is not None:
        max_buying_power_notional = available_buying_power * Decimal(
            str(config.max_buying_power_pct)
        )
        if order_notional > max_buying_power_notional:
            return (
                "RISK_MAX_BUYING_POWER_PCT: "
                f"order_notional={order_notional} "
                f"max_buying_power_notional={max_buying_power_notional}"
            )

    if order_notional > available_buying_power:
        return (
            "RISK_BUYING_POWER: "
            f"order_notional={order_notional} available_buying_power={available_buying_power}"
        )

    return None


def _equity_limit_payload_from_order(order: Order) -> dict[str, str]:
    error = _validate_equity_limit_order(order)
    if error is not None:
        raise ValueError(error)
    return {
        "symbol": order.instrument_id.symbol.value,
        "qty": str(order.quantity),
        "side": "buy" if order.side == OrderSide.BUY else "sell",
        "type": "limit",
        "time_in_force": "day",
        "limit_price": str(order.price),
        "client_order_id": str(order.client_order_id),
    }


def _equity_open_order_rows(open_order: dict[str, Any]) -> list[dict[str, Any]]:
    rows = nested_order_legs(open_order) or [open_order]
    equity_rows: list[dict[str, Any]] = []
    for row in rows:
        symbol = row.get("symbol")
        if not symbol:
            continue
        if is_alpaca_option_symbol(str(symbol)):
            continue
        equity_rows.append(row)
    return equity_rows


def _equity_position_risk_maps(
    positions: list[Any],
) -> tuple[dict[str, Decimal], dict[str, Decimal]]:
    qty_by_symbol: dict[str, Decimal] = {}
    notional_by_symbol: dict[str, Decimal] = {}
    for position in positions:
        if not isinstance(position, dict):
            continue
        symbol = normalize_alpaca_symbol(_required_str(position, "symbol"))
        if is_alpaca_option_symbol(symbol):
            continue
        quantity = _signed_position_qty_from_alpaca(position)
        avg_entry_price = _decimal_from_optional(position.get("avg_entry_price")) or Decimal(0)
        qty_by_symbol[symbol] = qty_by_symbol.get(symbol, Decimal(0)) + quantity
        notional_by_symbol[symbol] = notional_by_symbol.get(symbol, Decimal(0)) + abs(
            quantity * avg_entry_price,
        )
    return qty_by_symbol, notional_by_symbol


def _equity_open_order_risk_maps(
    open_orders: list[Any],
) -> tuple[dict[tuple[str, OrderSide], Decimal], dict[str, Decimal], set[ClientOrderId]]:
    qty_by_symbol_side: dict[tuple[str, OrderSide], Decimal] = {}
    notional_by_symbol: dict[str, Decimal] = {}
    client_order_ids: set[ClientOrderId] = set()
    for open_order in open_orders:
        if not isinstance(open_order, dict):
            continue
        client_order_id = _client_order_id(open_order)
        if client_order_id is not None:
            client_order_ids.add(client_order_id)
        for order_row in _equity_open_order_rows(open_order):
            _accumulate_equity_open_order_risk(
                order_row,
                qty_by_symbol_side,
                notional_by_symbol,
                client_order_ids,
            )
    return qty_by_symbol_side, notional_by_symbol, client_order_ids


def _accumulate_equity_open_order_risk(
    order_row: dict[str, Any],
    qty_by_symbol_side: dict[tuple[str, OrderSide], Decimal],
    notional_by_symbol: dict[str, Decimal],
    client_order_ids: set[ClientOrderId],
) -> None:
    symbol = normalize_alpaca_symbol(_required_str(order_row, "symbol"))
    side = _order_side_from_alpaca(_required_str(order_row, "side"))
    quantity = _required_decimal(order_row, "qty")
    price = (
        _decimal_from_optional(order_row.get("limit_price"))
        or _decimal_from_optional(order_row.get("stop_price"))
        or Decimal(0)
    )
    qty_by_symbol_side[(symbol, side)] = (
        qty_by_symbol_side.get((symbol, side), Decimal(0)) + quantity
    )
    notional_by_symbol[symbol] = notional_by_symbol.get(symbol, Decimal(0)) + abs(
        quantity * price,
    )
    client_order_id = _client_order_id(order_row)
    if client_order_id is not None:
        client_order_ids.add(client_order_id)


def _order_qty(order: Order) -> Decimal:
    return Decimal(str(order.quantity))


def _order_notional(order: Order) -> Decimal:
    price = getattr(order, "price", None)
    if price is None:
        return Decimal(0)
    return abs(_order_qty(order) * Decimal(str(price)))


def _position_notional(position: Any) -> Decimal:
    return abs(_signed_position_qty(position) * Decimal(str(position.avg_px_open)))


def _signed_position_qty(position: Any) -> Decimal:
    quantity = Decimal(str(position.quantity))
    if position.side == PositionSide.SHORT:
        return -quantity
    return quantity


def _signed_position_qty_from_alpaca(data: dict[str, Any]) -> Decimal:
    quantity = _required_decimal(data, "qty")
    side = _position_side_from_alpaca(data.get("side"), quantity)
    if side == PositionSide.SHORT:
        return -abs(quantity)
    return abs(quantity)


def _account_balance_from_alpaca(data: dict[str, Any]) -> AccountBalance:
    currency_code = str(data.get("currency") or "USD").upper()
    currency = USD if currency_code == "USD" else Currency.from_str(currency_code)
    total = Money(_first_decimal(data, "equity", "portfolio_value", "cash"), currency)
    free = Money(
        _first_decimal(data, "options_buying_power", "buying_power", "cash", default=Decimal(0)),
        currency,
    )
    locked_decimal = total.as_decimal() - free.as_decimal()
    return AccountBalance(total=total, locked=Money(locked_decimal, currency), free=free)


def _decode_response(status: int, body: bytes) -> Any:
    if not 200 <= status < 300:
        text = body.decode("utf-8", errors="replace")
        raise RuntimeError(f"Alpaca REST request failed: HTTP {status} {text}")
    if not body:
        return None
    return msgspec.json.decode(body)


def _first_decimal(
    data: dict[str, Any],
    *keys: str,
    default: Decimal | None = None,
) -> Decimal:
    for key in keys:
        value = _decimal_from_optional(data.get(key))
        if value is not None:
            return value
    if default is not None:
        return default
    raise ValueError(f"Alpaca response missing one of {keys}")


def _decimal_from_optional(value: Any) -> Decimal | None:
    if value is None:
        return None
    text = str(value).strip()
    if not text:
        return None
    return Decimal(text)


def _required_decimal(data: dict[str, Any], key: str) -> Decimal:
    value = _decimal_from_optional(data.get(key))
    if value is None:
        raise ValueError(f"Alpaca response missing {key}")
    return value


def _required_str(data: dict[str, Any], key: str) -> str:
    value = data.get(key)
    if value is None or str(value).strip() == "":
        raise ValueError(f"Alpaca response missing {key}")
    return str(value)


def _quantity_from_decimal(instrument: Instrument, value: Decimal) -> Quantity:
    return instrument.make_qty(value)


def _order_side_from_alpaca(value: str) -> OrderSide:
    normalized = value.lower()
    if normalized == "buy":
        return OrderSide.BUY
    if normalized == "sell":
        return OrderSide.SELL
    raise ValueError(f"Unsupported Alpaca order side {value!r}")


def _order_type_from_alpaca(value: str) -> OrderType:
    normalized = value.lower()
    if normalized == "limit":
        return OrderType.LIMIT
    if normalized == "market":
        return OrderType.MARKET
    if normalized == "stop":
        return OrderType.STOP_MARKET
    if normalized == "stop_limit":
        return OrderType.STOP_LIMIT
    raise ValueError(f"Unsupported Alpaca order type {value!r}")


def _time_in_force_from_alpaca(value: str) -> TimeInForce:
    normalized = value.lower()
    if normalized == "day":
        return TimeInForce.DAY
    if normalized == "gtc":
        return TimeInForce.GTC
    if normalized == "ioc":
        return TimeInForce.IOC
    if normalized == "fok":
        return TimeInForce.FOK
    raise ValueError(f"Unsupported Alpaca time_in_force {value!r}")


def _order_status_from_alpaca(value: Any) -> OrderStatus:
    normalized = str(value or "").lower()
    if not normalized:
        raise ValueError("Alpaca order missing status")
    status = _ORDER_STATUS_BY_ALPACA_STATUS.get(normalized)
    if status is None:
        raise ValueError(f"Unsupported Alpaca order status {value!r}")
    return status


def _position_side_from_alpaca(value: Any, quantity: Decimal) -> PositionSide:
    normalized = str(value or "").lower()
    if normalized == "long" or quantity > 0:
        return PositionSide.LONG
    if normalized == "short" or quantity < 0:
        return PositionSide.SHORT
    return PositionSide.FLAT


def _venue_order_id(data: dict[str, Any]) -> VenueOrderId | None:
    value = data.get("id")
    return VenueOrderId(str(value)) if value else None


def _client_order_id(data: dict[str, Any]) -> ClientOrderId | None:
    value = data.get("client_order_id")
    return ClientOrderId(str(value)) if value else None


def _matches_client_order_id_prefix(data: dict[str, Any], prefix: str) -> bool:
    value = data.get("client_order_id")
    return isinstance(value, str) and value.startswith(prefix)


def _alpaca_rejected_reason(data: dict[str, Any]) -> str:
    reason = (
        data.get("rejected_reason")
        or data.get("reject_reason")
        or data.get("status_message")
        or data.get("message")
    )
    return str(reason) if reason else "Alpaca order rejected"


def _timestamp_ns_from_order(data: dict[str, Any], default: int) -> int:
    return timestamp_ns_from_value(
        data.get("updated_at")
        or data.get("filled_at")
        or data.get("canceled_at")
        or data.get("expired_at")
        or data.get("submitted_at")
        or data.get("created_at"),
        default,
    )
