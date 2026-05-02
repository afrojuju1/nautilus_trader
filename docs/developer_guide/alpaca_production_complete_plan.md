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
- Native `index_put_credit_entry` and `index_call_credit_entry` scanner/entry paths run from the
  Rust Alpaca runner and submit through Nautilus `SubmitOrderList` when explicitly enabled.
- Initial credit-spread management can cancel stale entries, evaluate close triggers, build
  reduce-only close MLegs, and mark filled closes in strategy state.
- The NUC has a supervised user service, external env file, lock, logs, health command, operator
  status command, and kill-switch/submission gates.

Known gaps:

- The `alpaca-index-put-credit-entry` runner is intentionally transitional and now mixes runtime
  config, strategy loop, broker I/O, and submission in one large binary. Continue moving reusable
  broker orchestration code into `src/` modules before live canary.
- Multi-day paper proof with real management closes is still outstanding.
- Websocket disconnect/reconnect and reconciliation events are wired, but they still need paper
  observation during an actual reconnect or broker event-loss scenario.

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

Status: complete.

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

Status: initial production-capable slice complete; continue validating edge broker lifecycle events
as paper/live proof expands.

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

Status: initial native runner complete. Paper submit proof exists for Nautilus MLeg submission; keep
strategy submission disabled by default outside intentional paper tests.

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

Status: initial management implementation complete. Full exit criteria still requires a paper run
that opens a spread, lets management close it, reconciles the close, and verifies zero residual
orders/positions.

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

Status: complete for supervised NUC proof. Production hardening remains in Phase 6.5.

## Phase 6: Operator Visibility And Alerts

Goal: operate the engine without reading raw logs as the primary interface.

Work:

- Add CLI/status output for account, orders, positions, strategy state, last scan, last decision, and
  last broker event. (Initial `alpaca-operator-status` binary complete with human and JSON output.)
- Add alerts for rejected orders, websocket disconnect, stale working orders, unmanaged positions,
  reconciliation mismatches, kill-switch activation, and service restart. (Initial alert summary is
  implemented in `alpaca-operator-status`; websocket disconnect alerts consume structured disconnect
  events when the live engine emits them.)
- Add structured event logs for strategy decisions and broker state transitions. (Runner emits
  structured `operator_event=` JSON for start, iteration, decision, and submit result events.)

Current status:

- `alpaca-index-credit-control.sh operator` reports service, account, orders, positions, strategy
  state, last scan, last decision, latest broker event, and alerts from one command.
- On 2026-05-02, operator status was verified on `ade-nucbox-k8-plus` with the user service active,
  kill-switch enabled, submission disabled, zero open orders, and zero positions.

Exit criteria:

- An operator can determine whether the engine is safe, idle, trading, blocked, or broken from one
  command.

Status: initial operator command complete. Remaining work is to feed more broker/runtime events into
the structured event stream and move the command to an installed release binary.

## Phase 6.5: Runtime Packaging And Architecture Hardening

Goal: turn the proven runner/deployment slice into a maintainable single account engine package.

Work:

- Build and install release binaries for `alpaca-index-put-credit-entry` and
  `alpaca-operator-status`; stop using `cargo run` from systemd and control scripts. (Complete:
  installer builds release binaries and service/control scripts execute installed binaries.)
- Move strategy state structs, atomic state persistence, operator-event emission, and credit-spread
  management helpers from the runner binary into reusable `src/` modules. (State schema,
  persistence, and operator events moved to `runtime`; close-decision helpers moved to
  `management`.)
- Make the bin targets thin entrypoints over library code so tests can cover runtime decisions
  without shelling out to binaries. (Partial: runtime state and management decisions have direct
  tests; runner remains a transitional binary.)
- Replace direct JSON state writes with atomic temp-file write plus rename.
- Share the strategy state schema between the runner and operator status command.
- Emit structured websocket disconnect/reconnect and broker reconciliation events from the Alpaca
  account websocket owner. (Initial disconnect, reconnect, reconciliation success, and
  reconciliation error events complete.)
- Refresh deployment docs so installed binaries, not Cargo commands, are the default operational
  path.

Exit criteria:

- The NUC service and control script use installed release binaries.
- The account engine can restart without requiring a source checkout build path.
- Strategy state persistence is atomic and read by both runtime and operator status through one
  shared schema.
- Runtime state and management decisions have direct Rust test coverage outside binary entrypoints.

Current status:

- Release binaries were built and installed on `ade-nucbox-k8-plus` on 2026-05-02.
- `alpaca-index-credit.service` was verified running `~/.local/bin/alpaca-index-put-credit-entry`
  instead of `cargo run`.
- `alpaca-index-credit-control.sh operator|health` was verified running
  `~/.local/bin/alpaca-operator-status`.
- Strategy state persistence is atomic and shared between runner and operator status through the
  `runtime` module.
- Credit-spread close decisions are shared through the `management` module with direct unit tests.
- Remaining Phase 6.5 work: extract broker orchestration helpers from the runner binary and
  paper-observe websocket reconnect/reconciliation events.

## Phase 7: Strategy Migration

Migration order:

1. `index_put_credit_entry` (initial native runner complete)
2. `index_call_credit_entry` (Phase 7A scanner path complete through
   `ALPACA_INDEX_CREDIT_STRATEGIES=call|both`)
3. `earnings_call_debit_entry` (debit-spread Alpaca MLeg order payload builders complete; scanner
   and earnings-event input policy still required)
4. `earnings_put_debit_entry` (debit-spread Alpaca MLeg order payload builders complete; scanner
   and earnings-event input policy still required)
5. Iron condors after 4-leg open/close is proven
6. Straddles/strangles after long-premium management rules are proven

Each strategy must have native open, management, close, reconciliation, paper validation, and
operator visibility before live enablement.

Phase 7 blockers:

- Earnings debit strategies need an approved earnings-calendar/input source and entry policy before
  scanner implementation. Do not infer earnings dates from broker option chains.
- Iron condors should wait until 4-leg open/close is paper-proven with the account engine.
- Straddles/strangles should wait until long-premium management and max-loss behavior are specified.

## Phase 8: Rollout

Rollout order:

1. Dry-run scanner only.
2. Paper submit with immediate cancel.
3. Paper submit with real management close.
4. Multi-day paper soak.
5. Tiny live canary.
6. Controlled expansion only after clean reconciliation and close behavior.

Pre-rollout gates:

- Phase 6.5 is complete.
- Paper account starts clean: active account, zero unmanaged positions, zero open orders unless the
  test intentionally creates them.
- Operator status reports `idle`, `blocked`, or `trading` accurately during the whole paper run and
  never hides a critical alert.
- Every submitted paper order is either reconciled into strategy state or canceled/closed and
  verified flat.
- Live canary remains disabled until the user explicitly enables live URLs and submission gates.

Current status:

- Blocked outside market hours for real paper execution proof.
- Latest safe broker check on 2026-05-02 showed active paper account, zero positions, and zero open
  orders after service verification.
- Tiny live canary remains blocked until explicit user approval, live endpoints, and live submission
  gates are enabled.

## Immediate Next Milestone

Finish the remaining Phase 6.5 broker-orchestration extraction and paper-observe websocket
reconnect/reconciliation events. After that, run the paper workflow during market hours: dry-run
scan, submit-with-cancel smoke, paper open with real management close, and multi-day paper soak.
