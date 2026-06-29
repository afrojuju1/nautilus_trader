# Alpaca Phase 2 Paper Proof

This checklist proves the Phase 2 adapter requirements against a real Alpaca paper account. Do not
claim Phase 2 execution proof unless these commands were run with paper credentials and the printed
account/order/position state was checked.

## Environment

Set paper credentials outside the repo:

```bash
export ALPACA_API_KEY=...
export ALPACA_SECRET_KEY=...
```

Optional endpoint overrides:

```bash
export ALPACA_TRADING_BASE_URL=https://paper-api.alpaca.markets
export ALPACA_TRADE_UPDATES_WS_URL=wss://paper-api.alpaca.markets/stream
```

## Preflight

Verify the account is active and no unmanaged option orders or positions are present:

```bash
cargo run -p nautilus-alpaca --features live --bin alpaca-ops -- account
```

Verify the trade-update stream authenticates:

```bash
cargo run -p nautilus-alpaca --features live --bin alpaca-watch-trade-updates -- 15
```

Verify startup reconciliation can reconstruct current broker state:

```bash
cargo run -p nautilus-alpaca --features live --bin alpaca-reconciliation-probe -- 240
```

Expected clean-idle output:

- `account.status=ACTIVE`
- `orders: open=0`
- `positions: total=0` unless intentionally testing open position recovery
- `mass_status` prints order/fill/position counts without errors

## Submit, Event, Cancel, And Repair Proof

Submit one tiny Nautilus `SubmitOrderList` MLeg spread, observe accepted/rejected events, and cancel
accepted orders:

```bash
ALPACA_ORDER_LIST_HARNESS_REPLACE_OPEN=true \
  cargo run -p nautilus-alpaca --features live --bin alpaca-submit-order-list-harness -- --scan SPY,QQQ,IWM 1
```

Then verify the account returns to a clean state:

```bash
cargo run -p nautilus-alpaca --features live --bin alpaca-ops -- account
cargo run -p nautilus-alpaca --features live --bin alpaca-reconciliation-probe -- 240
```

Acceptance criteria:

- The harness prints accepted or rejected Nautilus order events.
- If accepted and replace proof is enabled, the harness replaces the parent MLeg limit before cleanup.
- If accepted, the harness requests cancellation.
- `check_account_orders` shows no unintended open orders or positions after cancellation.
- `reconciliation_probe` succeeds and reports the broker order/fill/position surface.

Market-hours revalidation note:

- On May 2, 2026, Alpaca paper accepted a parent MLeg and both legs, but rejected the replace call
  with `cannot replace order in accepted status`; cleanup then canceled the parent and final account
  checks showed zero open orders and zero positions.
- On May 4, 2026 during regular market hours, Alpaca paper accepted a SPY put-credit MLeg,
  accepted both legs, and accepted a parent replace from `-0.47` to `-0.46` with returned
  status `new`. The original cleanup path attempted to cancel the replaced parent, so the active
  replacement parent was canceled manually; final account checks showed zero open orders, zero
  positions, and zero fills.
- On May 4, 2026 the installed account engine opened SPY and QQQ put-credit spreads under paper,
  then `ALPACA_FORCE_FLATTEN=true` submitted real paper close MLegs. QQQ filled immediately; SPY
  required stale close cancellation and repricing before filling. Final account checks showed zero
  open orders, zero positions, and both strategy entries closed in state.
- On May 4, 2026 the call-credit scanner selected QQQ, submitted
  `QQQ260512C00685000/QQQ260512C00687000` for `0.54` credit, Alpaca accepted both legs, and the
  paper account held one managed QQQ call-credit spread with zero open orders. The runtime was then
  restored to `strategies = ["put", "call"]`, `max_active_entries = 1`, `max_daily_submits = 1`,
  and `max_open_orders = 1`.
- After the May 4, 2026 market close, a one-shot engine pass temporarily made the managed QQQ call
  spread eligible by `max_hold`; `management.close_regular_hours_only = true` blocked the close with
  `outside_close_window`, and the broker account still showed zero open orders.
- On May 5, 2026 after the service hardening install, the same one-shot after-hours gate proof was
  repeated on the installed binary. The engine emitted `management_block` with
  `reason=outside_close_window`; account checks still showed zero open orders and the managed QQQ
  call-credit position.

## Standard Lifecycle Proof

Use the Python options `TradingNode` example with explicit symbols when you need to test a known
paper order lifecycle:

```bash
python examples/live/alpaca/options_mleg_trading_node.py \
  --broker-paper \
  --confirm-submit \
  --cancel-after-submit \
  --short-symbol <SHORT_PUT_SYMBOL> \
  --long-symbol <LONG_PUT_SYMBOL> \
  --short-leg-limit 0.50 \
  --long-leg-limit 0.10 \
  --qty 1
```

Acceptance criteria:

- Accepted/canceled/rejected/filled status is visible through normal execution reports.
- Account activities are paginated and matching fills are counted when fills occur.
- Final account surface is printed and checked.

## Lifecycle Cases Still Requiring Captured Broker Payloads

The adapter subscribes to `FILL`, `OPASN`, `OPEXP`, `OPEXC`, and `OPTRD` activities for
repair. Before live enablement, save real paper or live-safe captured payloads for:

- assignment
- exercise
- expiry
- rejected MLeg
- partial fill
- full fill

Use those captured payloads to add deterministic fixture tests before expanding beyond paper.
