#!/usr/bin/env python3
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
Submit and cancel one tiny Alpaca paper equity order.

This is intentionally a broker-paper smoke harness. It submits a real paper order and then requests
cancellation before checking account, position, and open-order state.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
from decimal import Decimal
from typing import Any

import msgspec

from nautilus_trader.adapters.alpaca.constants import ALPACA_API_KEY_ENV
from nautilus_trader.adapters.alpaca.constants import ALPACA_API_SECRET_ENV
from nautilus_trader.adapters.alpaca.constants import ALPACA_PAPER_TRADING_BASE_URL
from nautilus_trader.adapters.alpaca.constants import ALPACA_SECRET_KEY_ENV
from nautilus_trader.adapters.alpaca.constants import APCA_API_KEY_ID_ENV
from nautilus_trader.adapters.alpaca.constants import APCA_API_SECRET_KEY_ENV
from nautilus_trader.adapters.alpaca.data import APCA_API_KEY_HEADER
from nautilus_trader.adapters.alpaca.data import APCA_API_SECRET_HEADER
from nautilus_trader.core import nautilus_pyo3
from nautilus_trader.core.uuid import UUID4


TERMINAL_STATUSES = {"canceled", "expired", "filled", "rejected"}


async def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--symbol", default=os.getenv("ALPACA_EQUITY_SMOKE_SYMBOL", "SPY"))
    parser.add_argument("--qty", type=Decimal, default=Decimal(1))
    parser.add_argument("--limit-price", type=Decimal, default=Decimal("1.00"))
    parser.add_argument("--side", choices=["buy", "sell"], default="buy")
    parser.add_argument("--poll-attempts", type=int, default=6)
    parser.add_argument("--poll-delay-secs", type=float, default=1.0)
    parser.add_argument("--confirm-submit", action="store_true")
    parser.add_argument("--allow-live", action="store_true")
    args = parser.parse_args()

    if not args.confirm_submit:
        raise SystemExit("Pass --confirm-submit to submit a real Alpaca paper order.")
    if args.qty <= 0:
        raise SystemExit("--qty must be positive.")
    if args.limit_price <= 0:
        raise SystemExit("--limit-price must be positive.")

    base_url = os.getenv("ALPACA_TRADING_BASE_URL", ALPACA_PAPER_TRADING_BASE_URL).rstrip("/")
    if base_url != ALPACA_PAPER_TRADING_BASE_URL and not args.allow_live:
        raise SystemExit(f"Refusing non-paper Alpaca endpoint without --allow-live: {base_url}")

    api_key = _first_present(APCA_API_KEY_ID_ENV, ALPACA_API_KEY_ENV)
    api_secret = _first_present(
        APCA_API_SECRET_KEY_ENV, ALPACA_SECRET_KEY_ENV, ALPACA_API_SECRET_ENV
    )
    if not api_key or not api_secret:
        raise SystemExit(
            "Alpaca paper credentials are required. Set APCA_API_KEY_ID and APCA_API_SECRET_KEY.",
        )

    headers = {
        APCA_API_KEY_HEADER: api_key,
        APCA_API_SECRET_HEADER: api_secret,
    }
    client = nautilus_pyo3.HttpClient(timeout_secs=30)
    client_order_id = f"nautilus-equity-smoke-{UUID4()}"
    payload = {
        "symbol": args.symbol.strip().upper(),
        "qty": str(args.qty),
        "side": args.side,
        "type": "limit",
        "time_in_force": "day",
        "limit_price": str(args.limit_price),
        "client_order_id": client_order_id,
    }

    account = await _get_json(client, base_url, "/v2/account", headers)
    submitted = await _post_json(client, base_url, "/v2/orders", headers, payload)
    venue_order_id = submitted["id"]
    queried = await _get_json(
        client,
        base_url,
        f"/v2/orders/{venue_order_id}",
        headers,
        {"nested": "true"},
    )

    cancel_requested = False
    if queried.get("status") not in TERMINAL_STATUSES:
        cancel_requested = True
        await _delete(client, base_url, f"/v2/orders/{venue_order_id}", headers)

    final_order = queried
    for _ in range(args.poll_attempts):
        final_order = await _get_json(
            client,
            base_url,
            f"/v2/orders/{venue_order_id}",
            headers,
            {"nested": "true"},
        )
        if final_order.get("status") in TERMINAL_STATUSES:
            break
        await asyncio.sleep(args.poll_delay_secs)

    positions = await _get_json(client, base_url, "/v2/positions", headers)
    open_orders = await _get_json(
        client,
        base_url,
        "/v2/orders",
        headers,
        {"status": "open", "limit": 100, "direction": "desc"},
    )
    smoke_open_orders = [
        order
        for order in open_orders
        if isinstance(order, dict) and order.get("client_order_id") == client_order_id
    ]

    print(
        json.dumps(
            {
                "account_status": account.get("status"),
                "buying_power": account.get("buying_power"),
                "client_order_id": client_order_id,
                "venue_order_id": venue_order_id,
                "submitted_status": submitted.get("status"),
                "queried_status": queried.get("status"),
                "cancel_requested": cancel_requested,
                "final_status": final_order.get("status"),
                "positions_count": len(positions) if isinstance(positions, list) else None,
                "smoke_open_orders": smoke_open_orders,
            },
            indent=2,
        ),
    )

    if smoke_open_orders:
        raise SystemExit("Smoke order still appears in open orders; inspect Alpaca paper account.")


async def _get_json(
    client: nautilus_pyo3.HttpClient,
    base_url: str,
    path: str,
    headers: dict[str, str],
    params: dict[str, Any] | None = None,
) -> Any:
    response = await client.get(f"{base_url}{path}", params=params, headers=headers)
    return _decode_response(response.status, response.body)


async def _post_json(
    client: nautilus_pyo3.HttpClient,
    base_url: str,
    path: str,
    headers: dict[str, str],
    payload: dict[str, str],
) -> Any:
    response = await client.post(
        f"{base_url}{path}",
        headers={**headers, "Content-Type": "application/json"},
        body=msgspec.json.encode(payload),
    )
    return _decode_response(response.status, response.body)


async def _delete(
    client: nautilus_pyo3.HttpClient,
    base_url: str,
    path: str,
    headers: dict[str, str],
) -> Any:
    response = await client.delete(f"{base_url}{path}", headers=headers)
    if response.status == 204:
        return None
    return _decode_response(response.status, response.body)


def _decode_response(status: int, body: bytes) -> Any:
    if not 200 <= status < 300:
        text = body.decode("utf-8", errors="replace")
        raise RuntimeError(f"Alpaca REST request failed: HTTP {status} {text}")
    if not body:
        return None
    return msgspec.json.decode(body)


def _first_present(*env_names: str) -> str | None:
    for env_name in env_names:
        value = os.getenv(env_name)
        if value:
            return value
    return None


if __name__ == "__main__":
    asyncio.run(main())
