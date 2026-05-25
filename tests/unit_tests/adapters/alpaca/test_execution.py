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
from types import SimpleNamespace

import pandas as pd
import pytest

from nautilus_trader.adapters.alpaca.config import AlpacaExecClientConfig
from nautilus_trader.adapters.alpaca.execution import _account_balance_from_alpaca
from nautilus_trader.adapters.alpaca.execution import _equity_limit_payload_from_order
from nautilus_trader.adapters.alpaca.execution import _equity_risk_denial_reason
from nautilus_trader.adapters.alpaca.execution import _order_notional
from nautilus_trader.adapters.alpaca.execution import _order_status_from_alpaca
from nautilus_trader.adapters.alpaca.execution import _timestamp_ns_from_value
from nautilus_trader.adapters.alpaca.execution import _validate_equity_limit_order
from nautilus_trader.core.datetime import dt_to_unix_nanos
from nautilus_trader.model.enums import OrderSide
from nautilus_trader.model.enums import OrderStatus
from nautilus_trader.model.enums import OrderType
from nautilus_trader.model.enums import TimeInForce
from nautilus_trader.model.identifiers import ClientOrderId
from nautilus_trader.model.identifiers import InstrumentId
from nautilus_trader.model.objects import Price
from nautilus_trader.model.objects import Quantity


def test_equity_limit_payload_matches_alpaca_day_limit_shape() -> None:
    order = _order()

    payload = _equity_limit_payload_from_order(order)

    assert payload == {
        "symbol": "SPY",
        "qty": "2",
        "side": "buy",
        "type": "limit",
        "time_in_force": "day",
        "limit_price": "101.23",
        "client_order_id": "O-001",
    }


def test_validate_equity_limit_order_rejects_non_day_tif() -> None:
    order = _order(time_in_force=TimeInForce.GTC)

    assert _validate_equity_limit_order(order) == "UNSUPPORTED_TIME_IN_FORCE: GTC"


def test_risk_gate_blocks_kill_switch() -> None:
    reason = _risk_reason(_order(), AlpacaExecClientConfig(risk_kill_switch=True))

    assert reason == "RISK_KILL_SWITCH"


def test_risk_gate_blocks_max_order_notional() -> None:
    reason = _risk_reason(
        _order(),
        AlpacaExecClientConfig(max_order_notional=Decimal(100)),
    )

    assert reason == "RISK_MAX_ORDER_NOTIONAL: order_notional=202.46 max_order_notional=100"


def test_risk_gate_blocks_duplicate_symbol_exposure_for_opening_order() -> None:
    reason = _risk_reason(
        _order(),
        AlpacaExecClientConfig(),
        same_symbol_exposure_exists=True,
    )

    assert reason == "RISK_DUPLICATE_SYMBOL_EXPOSURE: SPY"


def test_risk_gate_allows_sell_that_closes_existing_long_quantity() -> None:
    reason = _risk_reason(
        _order(side=OrderSide.SELL),
        AlpacaExecClientConfig(),
        current_symbol_position_qty=Decimal(5),
    )

    assert reason is None


def test_risk_gate_blocks_sell_that_would_open_short_by_default() -> None:
    reason = _risk_reason(
        _order(side=OrderSide.SELL),
        AlpacaExecClientConfig(),
        current_symbol_position_qty=Decimal(1),
    )

    assert reason == "RISK_SHORT_SELLING_DISABLED: sell_qty=2 closeable_long_qty=1"


def test_risk_gate_blocks_buying_power_pct() -> None:
    reason = _risk_reason(
        _order(),
        AlpacaExecClientConfig(max_buying_power_pct=0.05),
        available_buying_power=Decimal(1000),
    )

    assert (
        reason == "RISK_MAX_BUYING_POWER_PCT: order_notional=202.46 max_buying_power_notional=50.00"
    )


def test_account_balance_uses_equity_and_buying_power() -> None:
    balance = _account_balance_from_alpaca(
        {
            "currency": "USD",
            "equity": "10000.00",
            "buying_power": "7500.00",
        },
    )

    assert balance.total.as_decimal() == 10000
    assert balance.free.as_decimal() == 7500
    assert balance.locked.as_decimal() == 2500


def test_account_balance_allows_margin_buying_power_above_equity() -> None:
    balance = _account_balance_from_alpaca(
        {
            "currency": "USD",
            "equity": "10000.00",
            "buying_power": "20000.00",
        },
    )

    assert balance.total.as_decimal() == 10000
    assert balance.free.as_decimal() == 20000
    assert balance.locked.as_decimal() == -10000


def test_order_status_mapping_handles_partial_and_terminal_statuses() -> None:
    assert _order_status_from_alpaca("partially_filled") == OrderStatus.PARTIALLY_FILLED
    assert _order_status_from_alpaca("filled") == OrderStatus.FILLED
    assert _order_status_from_alpaca("canceled") == OrderStatus.CANCELED
    assert _order_status_from_alpaca("done_for_day") == OrderStatus.EXPIRED
    assert _order_status_from_alpaca("replaced") == OrderStatus.PENDING_UPDATE
    assert _order_status_from_alpaca("suspended") == OrderStatus.REJECTED


def test_order_status_mapping_rejects_unknown_statuses() -> None:
    with pytest.raises(ValueError, match="Unsupported Alpaca order status"):
        _order_status_from_alpaca("not-a-real-status")


def test_timestamp_parsing_uses_utc_nanoseconds() -> None:
    timestamp = "2026-05-24T14:30:00Z"

    assert _timestamp_ns_from_value(timestamp, 0) == dt_to_unix_nanos(pd.Timestamp(timestamp))


def _order(**overrides):
    values = {
        "instrument_id": InstrumentId.from_str("SPY.ALPACA"),
        "order_type": OrderType.LIMIT,
        "time_in_force": TimeInForce.DAY,
        "is_quote_quantity": False,
        "side": OrderSide.BUY,
        "quantity": Quantity.from_int(2),
        "price": Price.from_str("101.23"),
        "client_order_id": ClientOrderId("O-001"),
    }
    values.update(overrides)
    return SimpleNamespace(**values)


def _risk_reason(
    order,
    config: AlpacaExecClientConfig,
    *,
    current_total_notional: Decimal = Decimal(0),
    current_symbol_position_qty: Decimal = Decimal(0),
    same_symbol_exposure_exists: bool = False,
    same_symbol_open_buy_qty: Decimal = Decimal(0),
    same_symbol_open_sell_qty: Decimal = Decimal(0),
    available_buying_power: Decimal | None = Decimal(10000),
) -> str | None:
    return _equity_risk_denial_reason(
        order=order,
        config=config,
        order_notional=_order_notional(order),
        current_total_notional=current_total_notional,
        current_symbol_position_qty=current_symbol_position_qty,
        same_symbol_exposure_exists=same_symbol_exposure_exists,
        same_symbol_open_buy_qty=same_symbol_open_buy_qty,
        same_symbol_open_sell_qty=same_symbol_open_sell_qty,
        available_buying_power=available_buying_power,
    )
