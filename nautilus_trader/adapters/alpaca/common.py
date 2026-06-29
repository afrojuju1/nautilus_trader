"""
Shared helpers for Alpaca Python adapter modules.
"""

from __future__ import annotations

import os
from typing import Any

import pandas as pd

from nautilus_trader.adapters.alpaca.constants import ALPACA_API_KEY_ENV
from nautilus_trader.adapters.alpaca.constants import ALPACA_API_SECRET_ENV
from nautilus_trader.adapters.alpaca.constants import ALPACA_SECRET_KEY_ENV
from nautilus_trader.adapters.alpaca.constants import APCA_API_KEY_ID_ENV
from nautilus_trader.adapters.alpaca.constants import APCA_API_SECRET_KEY_ENV
from nautilus_trader.core.datetime import dt_to_unix_nanos


APCA_API_KEY_HEADER = "APCA-API-KEY-ID"
APCA_API_SECRET_HEADER = "APCA-API-SECRET-KEY"  # noqa: S105


def resolve_alpaca_credentials(
    api_key: str | None,
    api_secret: str | None,
) -> tuple[str | None, str | None]:
    return (
        first_present(api_key, APCA_API_KEY_ID_ENV, ALPACA_API_KEY_ENV),
        first_present(
            api_secret,
            APCA_API_SECRET_KEY_ENV,
            ALPACA_SECRET_KEY_ENV,
            ALPACA_API_SECRET_ENV,
        ),
    )


def alpaca_auth_headers(
    api_key: str | None,
    api_secret: str | None,
    *,
    surface: str,
    config_name: str,
) -> dict[str, str]:
    if not api_key or not api_secret:
        raise RuntimeError(
            f"Alpaca {surface} credentials are required. Set APCA_API_KEY_ID and "
            f"APCA_API_SECRET_KEY, or pass api_key/api_secret in {config_name}.",
        )
    return {
        APCA_API_KEY_HEADER: api_key,
        APCA_API_SECRET_HEADER: api_secret,
    }


def first_present(explicit: str | None, *env_names: str) -> str | None:
    if explicit:
        return explicit

    for env_name in env_names:
        value = os.getenv(env_name)
        if value:
            return value
    return None


def normalize_alpaca_symbol(symbol: str) -> str:
    normalized = symbol.strip().upper()
    if not normalized:
        raise ValueError("symbol must not be empty")
    return normalized


def timestamp_ns_from_value(value: Any, default: int) -> int:
    if value is None:
        return default
    timestamp = pd.Timestamp(value)
    if timestamp.tzinfo is None:
        timestamp = timestamp.tz_localize("UTC")
    else:
        timestamp = timestamp.tz_convert("UTC")
    return dt_to_unix_nanos(timestamp)


def format_alpaca_datetime(value: Any) -> str:
    timestamp = pd.Timestamp(value)
    if timestamp.tzinfo is None:
        timestamp = timestamp.tz_localize("UTC")
    else:
        timestamp = timestamp.tz_convert("UTC")
    return timestamp.isoformat().replace("+00:00", "Z")
