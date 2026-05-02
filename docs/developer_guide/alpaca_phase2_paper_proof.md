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
cargo run -p nautilus-alpaca --features live --bin alpaca-check-account-orders
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
cargo run -p nautilus-alpaca --features live --bin alpaca-check-account-orders
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
- Re-run this proof during regular market hours with `ALPACA_ORDER_LIST_HARNESS_REPLACE_OPEN=true`
  and record whether Alpaca allows replacing the parent MLeg once the broker state is replaceable.

## Direct Lifecycle Proof

Use explicit symbols when you need to test a known order lifecycle:

```bash
cargo run -p nautilus-alpaca --features live --bin alpaca-paper-execution-harness -- \
  <SHORT_PUT_SYMBOL> <LONG_PUT_SYMBOL> <CREDIT_LIMIT> 1
```

Tune polling for fill or cancel scenarios:

```bash
export ALPACA_EXECUTION_POLL_ATTEMPTS=10
export ALPACA_EXECUTION_POST_CANCEL_POLL_ATTEMPTS=5
export ALPACA_EXECUTION_POLL_SECS=2
```

Acceptance criteria:

- Accepted/canceled/rejected/filled status is visible from REST polling.
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
