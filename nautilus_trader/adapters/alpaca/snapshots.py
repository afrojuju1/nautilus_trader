"""
Alpaca snapshot conversion helpers.
"""

from typing import Any

import pandas as pd

from nautilus_trader.core.datetime import dt_to_unix_nanos
from nautilus_trader.model.data import OptionGreeks
from nautilus_trader.model.data import QuoteTick
from nautilus_trader.model.identifiers import InstrumentId
from nautilus_trader.model.instruments import Instrument


def snapshot_for_symbol(payload: Any, symbol: str) -> dict[str, Any] | None:
    if not isinstance(payload, dict):
        return None
    snapshots = payload.get("snapshots")
    if not isinstance(snapshots, dict):
        return None
    snapshot = snapshots.get(symbol) or snapshots.get(symbol.upper())
    return snapshot if isinstance(snapshot, dict) else None


def quote_tick_from_option_snapshot(
    instrument: Instrument,
    snapshot: dict[str, Any],
    ts_init: int,
) -> QuoteTick | None:
    quote = _nested_dict(snapshot, "latestQuote", "latest_quote")
    if quote is None:
        return None

    bid_price = _first_float(quote, "bp", "bid_price", "bidPrice")
    ask_price = _first_float(quote, "ap", "ask_price", "askPrice")
    if bid_price is None or ask_price is None or bid_price <= 0 or ask_price <= 0:
        return None
    if ask_price < bid_price:
        return None

    bid_size = _first_int(quote, "bs", "bid_size", "bidSize", default=0)
    ask_size = _first_int(quote, "as", "ask_size", "askSize", default=0)
    ts_event = _timestamp_ns_from_value(quote.get("t") or quote.get("timestamp"), ts_init)
    return QuoteTick(
        instrument_id=instrument.id,
        bid_price=instrument.make_price(bid_price),
        ask_price=instrument.make_price(ask_price),
        bid_size=instrument.make_qty(bid_size),
        ask_size=instrument.make_qty(ask_size),
        ts_event=ts_event,
        ts_init=ts_init,
    )


def greeks_from_option_snapshot(
    instrument_id: InstrumentId,
    snapshot: dict[str, Any],
    ts_init: int,
) -> OptionGreeks | None:
    greeks = _nested_dict(snapshot, "greeks")
    mark_iv = _first_float(snapshot, "impliedVolatility", "implied_volatility", "iv")
    if greeks is None and mark_iv is None:
        return None
    greeks = greeks or {}

    quote = _nested_dict(snapshot, "latestQuote", "latest_quote") or {}
    ts_event = _timestamp_ns_from_value(
        quote.get("t") or quote.get("timestamp") or snapshot.get("updated_at"),
        ts_init,
    )
    return OptionGreeks(
        instrument_id=instrument_id,
        delta=_first_float(greeks, "d", "delta", default=0.0),
        gamma=_first_float(greeks, "g", "gamma", default=0.0),
        vega=_first_float(greeks, "v", "vega", default=0.0),
        theta=_first_float(greeks, "t", "theta", default=0.0),
        rho=_first_float(greeks, "r", "rho", default=0.0),
        mark_iv=mark_iv,
        bid_iv=None,
        ask_iv=None,
        underlying_price=_first_float(snapshot, "underlyingPrice", "underlying_price"),
        open_interest=_first_float(snapshot, "openInterest", "open_interest"),
        ts_event=ts_event,
        ts_init=ts_init,
    )


def _nested_dict(data: dict[str, Any], *keys: str) -> dict[str, Any] | None:
    for key in keys:
        value = data.get(key)
        if isinstance(value, dict):
            return value
    return None


def _first_float(
    data: dict[str, Any],
    *keys: str,
    default: float | None = None,
) -> float | None:
    for key in keys:
        value = data.get(key)
        if value is None or value == "":
            continue
        try:
            return float(value)
        except (TypeError, ValueError):
            continue
    return default


def _first_int(
    data: dict[str, Any],
    *keys: str,
    default: int = 0,
) -> int:
    for key in keys:
        value = data.get(key)
        if value is None or value == "":
            continue
        try:
            return int(value)
        except (TypeError, ValueError):
            continue
    return default


def _timestamp_ns_from_value(value: Any, default: int) -> int:
    if value is None:
        return default
    timestamp = pd.Timestamp(value)
    if timestamp.tzinfo is None:
        timestamp = timestamp.tz_localize("UTC")
    else:
        timestamp = timestamp.tz_convert("UTC")
    return dt_to_unix_nanos(timestamp)
