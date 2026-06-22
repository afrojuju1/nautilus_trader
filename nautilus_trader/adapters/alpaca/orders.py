"""
Alpaca order payload helpers for the Python live adapter.
"""

import math
from decimal import Decimal
from typing import Any

from nautilus_trader.adapters.alpaca.constants import ALPACA_VENUE
from nautilus_trader.adapters.alpaca.providers import is_alpaca_option_symbol
from nautilus_trader.execution.messages import SubmitOrderList
from nautilus_trader.model.enums import OrderSide
from nautilus_trader.model.enums import OrderType
from nautilus_trader.model.enums import TimeInForce
from nautilus_trader.model.orders import Order


def validate_mleg_order_list(command: SubmitOrderList) -> str | None:
    orders = list(command.order_list.orders)
    if len(orders) < 2:
        return "MLEG_REQUIRES_AT_LEAST_TWO_LEGS"
    if len(orders) > 4:
        return "MLEG_SUPPORTS_AT_MOST_FOUR_LEGS"

    all_reduce_only = all(order.is_reduce_only for order in orders)
    any_reduce_only = any(order.is_reduce_only for order in orders)
    if any_reduce_only and not all_reduce_only:
        return "MLEG_MIXED_OPEN_CLOSE_LEGS"

    for order in orders:
        error = _validate_mleg_leg_order(order)
        if error is not None:
            return error
    return None


def mleg_payload_from_order_list(command: SubmitOrderList) -> dict[str, Any]:
    error = validate_mleg_order_list(command)
    if error is not None:
        raise ValueError(error)

    orders = list(command.order_list.orders)
    quantities = [int(_order_qty(order)) for order in orders]
    strategy_qty = math.gcd(*quantities)
    if strategy_qty <= 0:
        raise ValueError("MLEG_STRATEGY_QUANTITY_MUST_BE_POSITIVE")

    trade_intent = "close" if all(order.is_reduce_only for order in orders) else "open"
    net_credit = Decimal(0)
    legs: list[dict[str, str]] = []
    for order, leg_qty in zip(orders, quantities, strict=True):
        ratio_qty = leg_qty // strategy_qty
        price = Decimal(str(order.price))
        net_credit += (price if order.side == OrderSide.SELL else -price) * Decimal(ratio_qty)
        position_intent = _alpaca_position_intent(order.side, order.is_reduce_only)
        legs.append(
            {
                "symbol": order.instrument_id.symbol.value,
                "ratio_qty": str(ratio_qty),
                "side": alpaca_order_side(order.side),
                "position_intent": position_intent,
            },
        )

    if net_credit == 0:
        raise ValueError("MLEG_SIGNED_NET_LIMIT_PRICE_MUST_BE_NON_ZERO")

    if trade_intent == "open":
        premium_kind = "credit" if net_credit > 0 else "debit"
    else:
        premium_kind = "credit" if net_credit < 0 else "debit"
    signed_limit_price = _signed_net_limit_price(abs(net_credit), premium_kind, trade_intent)
    return {
        "order_class": "mleg",
        "client_order_id": str(command.order_list.id),
        "qty": str(strategy_qty),
        "type": "limit",
        "limit_price": _format_price_decimal(signed_limit_price),
        "time_in_force": "day",
        "legs": legs,
    }


def mleg_leg_snapshot_for_order(order: Order, parent: dict[str, Any]) -> dict[str, Any]:
    leg = _matching_mleg_leg(order, nested_order_legs(parent))
    snapshot = mleg_report_leg_snapshot(parent, leg or {})
    parent_id = str(parent.get("id") or "UNKNOWN")
    snapshot["id"] = str(snapshot.get("id") or f"{parent_id}:{order.client_order_id}")
    snapshot["symbol"] = order.instrument_id.symbol.value
    snapshot["qty"] = str(order.quantity)
    snapshot["side"] = alpaca_order_side(order.side)
    snapshot["type"] = "limit"
    snapshot["time_in_force"] = "day"
    snapshot["limit_price"] = str(order.price)
    snapshot["client_order_id"] = str(order.client_order_id)
    snapshot.setdefault("filled_qty", "0")
    return snapshot


def mleg_report_leg_snapshot(parent: dict[str, Any], leg: dict[str, Any]) -> dict[str, Any]:
    snapshot = {
        key: parent[key]
        for key in (
            "status",
            "type",
            "time_in_force",
            "created_at",
            "updated_at",
            "submitted_at",
            "canceled_at",
            "expired_at",
            "filled_at",
        )
        if key in parent
    }
    snapshot.update({key: value for key, value in leg.items() if value is not None})
    parent_id = parent.get("id")
    if not snapshot.get("id") and parent_id and snapshot.get("symbol"):
        snapshot["id"] = f"{parent_id}:{snapshot['symbol']}:{snapshot.get('side', '')}"
    snapshot.setdefault("type", "limit")
    snapshot.setdefault("time_in_force", "day")
    snapshot.setdefault("filled_qty", "0")
    return snapshot


def nested_order_legs(data: dict[str, Any]) -> list[dict[str, Any]]:
    legs = data.get("legs")
    if not isinstance(legs, list):
        return []
    return [leg for leg in legs if isinstance(leg, dict)]


def alpaca_order_side(side: OrderSide) -> str:
    if side == OrderSide.BUY:
        return "buy"
    if side == OrderSide.SELL:
        return "sell"
    raise ValueError(f"Unsupported Alpaca order side {side}")


def _validate_mleg_leg_order(order: Order) -> str | None:
    if order.instrument_id.venue != ALPACA_VENUE:
        return f"UNSUPPORTED_VENUE: {order.instrument_id.venue}"
    if not is_alpaca_option_symbol(order.instrument_id.symbol.value):
        return f"MLEG_LEG_REQUIRES_OPTION_SYMBOL: {order.instrument_id.symbol.value}"
    if order.order_type != OrderType.LIMIT:
        return f"UNSUPPORTED_ORDER_TYPE: {order.order_type.name}"
    if order.time_in_force != TimeInForce.DAY:
        return f"UNSUPPORTED_TIME_IN_FORCE: {order.time_in_force.name}"
    if order.side not in (OrderSide.BUY, OrderSide.SELL):
        return f"UNSUPPORTED_ORDER_SIDE: {order.side.name}"
    if order.is_quote_quantity:
        return "UNSUPPORTED_QUOTE_QUANTITY"
    quantity = _order_qty(order)
    if quantity <= 0 or quantity != quantity.to_integral_value():
        return "MLEG_QUANTITY_MUST_BE_POSITIVE_INTEGER_CONTRACTS"
    if order.price is None or Decimal(str(order.price)) <= 0:
        return "INVALID_LIMIT_PRICE"
    return None


def _alpaca_position_intent(side: OrderSide, reduce_only: bool) -> str:
    if side == OrderSide.BUY and not reduce_only:
        return "buy_to_open"
    if side == OrderSide.SELL and not reduce_only:
        return "sell_to_open"
    if side == OrderSide.BUY and reduce_only:
        return "buy_to_close"
    if side == OrderSide.SELL and reduce_only:
        return "sell_to_close"
    raise ValueError(f"Unsupported Alpaca order side {side}")


def _signed_net_limit_price(
    limit_price: Decimal,
    premium_kind: str,
    trade_intent: str,
) -> Decimal:
    normalized_limit = abs(limit_price)
    if (premium_kind, trade_intent) in (("credit", "open"), ("debit", "close")):
        return -normalized_limit
    return normalized_limit


def _format_price_decimal(value: Decimal) -> str:
    return str(value.quantize(Decimal("0.01")))


def _matching_mleg_leg(order: Order, legs: list[dict[str, Any]]) -> dict[str, Any] | None:
    for leg in legs:
        if _mleg_leg_matches_order(order, leg):
            return leg
    return None


def _mleg_leg_matches_order(order: Order, leg: dict[str, Any]) -> bool:
    symbol = leg.get("symbol")
    if symbol is None:
        return False
    if _normalize_symbol(str(symbol)) != _normalize_symbol(order.instrument_id.symbol.value):
        return False
    side = leg.get("side")
    if side is not None and _order_side_from_alpaca(str(side)) != order.side:
        return False
    position_intent = leg.get("position_intent")
    return position_intent is None or str(position_intent).lower() == _alpaca_position_intent(
        order.side,
        order.is_reduce_only,
    )


def _order_side_from_alpaca(value: str) -> OrderSide:
    normalized = value.lower()
    if normalized == "buy":
        return OrderSide.BUY
    if normalized == "sell":
        return OrderSide.SELL
    raise ValueError(f"Unsupported Alpaca order side {value!r}")


def _order_qty(order: Order) -> Decimal:
    return Decimal(str(order.quantity))


def _normalize_symbol(symbol: str) -> str:
    normalized = symbol.strip().upper()
    if not normalized:
        raise ValueError("symbol must not be empty")
    return normalized
