# Alpaca Production Complete Plan

This document is the high-level plan for making this fork the production trading engine for the
Alpaca option-spread workflow. The target state is Nautilus-native execution and strategy runtime.
`spreads` is legacy/reference material only and must not remain in the live trading loop.

## Target State

- Nautilus owns live strategy runtime, Alpaca market data access, Alpaca execution, order events,
  reconciliation, position lifecycle, and operator controls.
- The NUC (`ade-nucbox-k8-plus`) runs one supervised Nautilus paper/live process for the active
  Alpaca account.
- That process is the single Alpaca account engine: it owns broker connectivity and account-level
  risk, while hosting multiple enabled strategies from config. Add separate supervised services only
  for separate ownership domains such as paper vs live accounts or non-trading diagnostics.
- Strategies are implemented as Nautilus-native strategies or Rust/Python components in this repo.
- Alpaca option spreads are submitted through Nautilus `SubmitOrderList` and reconciled through
  trade updates plus REST repair paths.
- Every production feature has paper proof before live enablement.

## Current State

Completed foundation:

- Alpaca adapter crate exists under `crates/adapters/alpaca`.
- Authenticated REST clients cover account, positions, orders, option contracts, option snapshots,
  order lookup, cancel, account activities, and MLeg submission.
- Alpaca execution client supports option-spread `SubmitOrderList`; single-order submit is denied
  for now.
- Trade-update websocket handling maps Alpaca parent and leg updates back to Nautilus leg client
  order IDs.
- Paper smoke tests have submitted an MLeg spread through Nautilus, observed accepted leg events,
  canceled the parent, and verified zero positions/open orders afterward.
- The fork has a documented upstream sync workflow in `AGENTS.md`.

Known gaps:

- No production Nautilus live strategy node is deployed on the NUC.
- The current put-credit loop scans and prints readiness; it does not run as a Nautilus strategy or
  submit entries.
- `generate_fill_reports` and `generate_position_status_reports` are not production-complete.
- Native close/management logic for vertical spreads is not implemented.
- The operational deployment story still needs a non-`spreads` env, service, logs, health, and kill
  switch.

## Phase 1: Foundation Lock

Goal: make the fork safe to build on and safe to sync.

Scope:

- Track `AGENTS.md` in the fork, even though upstream ignores local agent instruction files.
- Keep the upstream sync workflow merge-based: merge `upstream/develop` into fork `develop`, never
  reset fork `develop` to upstream.
- Maintain a production-complete roadmap document in this repo.
- Keep the repeated adapter verification commands explicit and easy to run.
- Preserve the fork-only Alpaca runtime commits during upstream syncs.

Required checks:

```bash
cargo fmt -p nautilus-alpaca
cargo check -p nautilus-alpaca --features live --bins
cargo test -p nautilus-alpaca --features live --lib
```

Exit criteria:

- `AGENTS.md` is committed and visible in fork history.
- This roadmap is committed and linked from the developer guide.
- The adapter checks above pass on the NUC.
- `origin/develop` contains upstream plus the fork-only Alpaca commits.

Status: complete once this document and `AGENTS.md` are committed and pushed.

## Phase 2: Production-Capable Alpaca Adapter

Goal: make the adapter safe after process restarts, websocket gaps, and broker lifecycle events.

Work:

- Implement real `generate_fill_reports` from Alpaca account activities.
- Implement real `generate_position_status_reports` from Alpaca positions.
- Harden startup mass status recovery for nested MLeg parent and leg orders.
- Make websocket trade-update handling idempotent across duplicate/replayed events.
- Add REST polling repair for missed trade-update events.
- Validate assignment, exercise, expiry, cancellation, rejection, partial fill, and filled lifecycle
  mapping.
- Add cancel/replace support for working MLeg entries where Alpaca supports it.
- Keep single-leg submit disabled unless a strategy explicitly needs it.

Exit criteria:

- A restarted Nautilus process can reconstruct account, orders, fills, and positions without
  `spreads`.
- Paper tests cover accepted, rejected, canceled, filled, and startup-reconciled MLeg orders.

## Phase 3: Native `index_put_credit_entry`

Goal: run the first strategy fully inside Nautilus.

Work:

- Build a Nautilus-native put-credit strategy or strategy runner. (Initial
  `alpaca-index-put-credit-entry` Rust runner complete; it scans, applies broker/state admission,
  selects one entry, and can submit through the Alpaca `SubmitOrderList` execution path when
  explicitly enabled.)
- Port scanner parameters for underlyings, DTE, width, delta, open interest, leg spread, and minimum
  return-on-risk. (Runner exposes these through `ALPACA_PUT_CREDIT_*` environment variables.)
- Add account/position/open-order admission checks. (Runner uses the Alpaca adapter admission gate.)
- Add deterministic sizing and daily duplicate-entry controls. (Runner uses
  `ALPACA_INDEX_PUT_CREDIT_QTY` and persists submitted entries by trade date and underlying.)
- Build and submit Nautilus `SubmitOrderList` entries directly. (Submission is disabled by default;
  set `ALPACA_INDEX_PUT_CREDIT_SUBMIT=true` for paper execution.)
- Persist enough local strategy state to avoid duplicate submits after restart. (Default state path
  is `$XDG_STATE_HOME/nautilus_trader/alpaca_index_put_credit_entry_state.json` or
  `$HOME/.local/state/nautilus_trader/alpaca_index_put_credit_entry_state.json`.)
- Remove any dependency on `spreads` execution intents, workers, or Docker runtime.

Exit criteria:

- One paper run scans, selects, submits, observes order events, and leaves a coherent Nautilus state.

Runner commands:

```bash
# Dry-run scan, no order submission.
ALPACA_INDEX_PUT_CREDIT_IGNORE_WINDOW=true \
  cargo run -p nautilus-alpaca --features live --bin alpaca-index-put-credit-entry -- SPY,QQQ,IWM

# Paper submission path; use cancel-after-accept only for smoke testing.
ALPACA_INDEX_PUT_CREDIT_SUBMIT=true \
ALPACA_INDEX_PUT_CREDIT_CANCEL_AFTER_ACCEPT=true \
  cargo run -p nautilus-alpaca --features live --bin alpaca-index-put-credit-entry -- SPY,QQQ,IWM
```

Phase 7A call-credit mode:

```bash
# Scan both index put-credit and index call-credit candidates; submission still disabled.
ALPACA_INDEX_PUT_CREDIT_IGNORE_WINDOW=true \
ALPACA_INDEX_CREDIT_STRATEGIES=both \
  cargo run -p nautilus-alpaca --features live --bin alpaca-index-put-credit-entry -- SPY,QQQ,IWM
```

Implementation note:

- The existing Python `OrderList` constructor enforces one `InstrumentId` per list, while Alpaca
  MLeg entries require distinct option leg instruments. Until that core model constraint is changed
  or a Python-safe multi-leg abstraction is added, the native Phase 3 submit path lives in the Rust
  runner where the Alpaca execution client already accepts multi-leg `SubmitOrderList` commands.
- Recommended submit path for Phase 3 and Phase 4: keep selection, admission, and MLeg submission
  in the Rust Alpaca runner, then expose a narrow Python control/status surface around it. Do not
  relax Python `OrderList` globally just for Alpaca; if Python-native submission becomes necessary,
  add an explicit multi-instrument order-list abstraction or an Alpaca-specific PyO3 submit helper
  after risk, cache, and execution semantics are designed for broker-native option spreads.

## Phase 4: Position Management And Close Path

Goal: make openings safe by owning the complete lifecycle.

Work:

- Add profit target, stop loss, max adverse move, stale order timeout, time-based exit, and
  expiration-risk exit. (Initial profit-target, stop-loss-debit, max-hold, stale-entry, and
  expiration-risk evaluation complete.)
- Implement close MLeg order-list construction for verticals. (Initial reduce-only close
  `SubmitOrderList` construction complete for credit verticals.)
- Add cancel stale entry and close orders. (Runner can cancel stale entry orders and submit close
  orders only when `ALPACA_INDEX_CREDIT_MANAGE=true`; close submission also requires
  `ALPACA_INDEX_CREDIT_CLOSE=true`.)
- Add manual flatten and global kill switch. (Initial `ALPACA_INDEX_CREDIT_FORCE_FLATTEN` and
  `ALPACA_INDEX_CREDIT_KILL_SWITCH` controls complete.)
- Reconcile close fills and mark positions flat. (Runner marks persisted entries closed after a
  filled close parent order is observed.)

Exit criteria:

- Nautilus can open, manage, close, and reconcile a vertical spread without manual database or
  broker-console intervention.

Management controls:

```bash
# Evaluate management without broker actions.
ALPACA_INDEX_CREDIT_MANAGE=false \
  cargo run -p nautilus-alpaca --features live --bin alpaca-index-put-credit-entry

# Allow stale-order cancel and close submission when triggers fire.
ALPACA_INDEX_CREDIT_MANAGE=true \
ALPACA_INDEX_CREDIT_CLOSE=true \
  cargo run -p nautilus-alpaca --features live --bin alpaca-index-put-credit-entry
```

Key controls:

- `ALPACA_INDEX_CREDIT_KILL_SWITCH=true` blocks new entries.
- `ALPACA_INDEX_CREDIT_FORCE_FLATTEN=true` treats every tracked open spread as a close candidate.
- `ALPACA_INDEX_CREDIT_STALE_ENTRY_SECS=900` cancels stale working entries when management is
  enabled.
- `ALPACA_INDEX_CREDIT_PROFIT_TARGET_CLOSE_FRACTION=0.50` closes when debit falls to 50% of entry
  credit.
- `ALPACA_INDEX_CREDIT_STOP_LOSS_CLOSE_MULTIPLE=2.0` closes when debit reaches 2x entry credit.
- `ALPACA_INDEX_CREDIT_EXPIRATION_EXIT_DAYS=1` closes near expiration risk.

## Phase 5: NUC Production Deployment

Goal: run Nautilus as the only live trading engine on the box.

Work:

- Create a NUC service definition for the Nautilus strategy runner.
- Store env/secrets outside the repo. (Initial `deploy/alpaca/alpaca-index-credit.env.example`
  documents the external env file shape without secrets.)
- Add paper/live mode, account ID, strategy config, and symbol universe config. (Initial env
  template covers paper URLs, strategy universe, scan cadence, and safety gates.)
- Add log files, rotation, restart policy, health command, status command, and stop command.
  (Initial user systemd unit, control script, runner wrapper, and logrotate example complete.)
- Ensure only one Alpaca account trade-update websocket owner is active. (Runner wrapper uses a
  non-blocking `flock` lock under `NAUTILUS_ALPACA_LOCK_DIR`.)
- Remove any `spreads` runtime dependency from the service.

Current status:

- Deployment files are documented in [Alpaca NUC Deployment](alpaca_nuc_deployment.md).
- On 2026-05-02, `alpaca-index-credit.service` was installed, started, health-checked, and stopped
  on `ade-nucbox-k8-plus` with kill-switch enabled and submission disabled.
- The runner supports `ALPACA_INDEX_PUT_CREDIT_MAX_ITERATIONS=0` for continuous service mode.

Exit criteria:

- The NUC can start, stop, restart, and report status for the Nautilus engine cleanly.

## Phase 6: Operator Visibility And Alerts

Goal: operate the engine without reading raw logs as the primary interface.

Work:

- Add CLI/status output for account, orders, positions, strategy state, last scan, last decision, and
  last broker event.
- Add alerts for rejected orders, websocket disconnect, stale working orders, unmanaged positions,
  reconciliation mismatches, kill-switch activation, and service restart.
- Add structured event logs for strategy decisions and broker state transitions.

Exit criteria:

- An operator can determine whether the engine is safe, idle, trading, blocked, or broken from one
  command.

## Phase 7: Strategy Migration

Migration order:

1. `index_put_credit_entry` (initial native runner complete)
2. `index_call_credit_entry` (Phase 7A scanner path complete through
   `ALPACA_INDEX_CREDIT_STRATEGIES=call|both`)
3. `earnings_call_debit_entry`
4. `earnings_put_debit_entry`
5. Iron condors after 4-leg open/close is proven
6. Straddles/strangles after long-premium management rules are proven

Each strategy must have native open, management, close, reconciliation, paper validation, and
operator visibility before live enablement.

## Phase 8: Rollout

Rollout order:

1. Dry-run scanner only.
2. Paper submit with immediate cancel.
3. Paper submit with real management close.
4. Multi-day paper soak.
5. Tiny live canary.
6. Controlled expansion only after clean reconciliation and close behavior.

## Immediate Next Milestone

The next milestone after Phase 1 is Phase 2 plus the first Phase 3 slice: implement the
Nautilus-native `index_put_credit_entry` runner while closing the adapter reconciliation gaps needed
to survive restart and broker event loss.
