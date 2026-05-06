# Alpaca Production Complete Plan

This document is the high-level plan for making this fork the production trading engine for the
Alpaca option-spread workflow. The target state is Nautilus-native execution and strategy runtime.
`spreads` is legacy/reference material only and must not remain in the live trading loop.

## Target State

- Nautilus owns live strategy runtime, Alpaca market data access, Alpaca execution, order events,
  reconciliation, position lifecycle, and operator controls.
- The NUC (`ade-nucbox-k8-plus`) runs one supervised Nautilus paper/live process for each enabled
  Alpaca account.
- Each process is a single Alpaca account engine: it owns broker connectivity and account-level
  risk, while hosting multiple enabled strategies from config. Add separate supervised services only
  for separate broker accounts or non-trading diagnostics.
- A read-only fleet registry tracks account role, permissions, risk budget, env file, config file,
  and service name. It does not allocate trades across accounts until a deliberate allocator is
  designed.
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
- Initial credit/debit spread management can cancel stale entries, evaluate close triggers with
  expiration-risk exits, emit management snapshots with PnL context, build reduce-only close MLegs,
  and mark filled closes in strategy state.
- The NUC has a supervised user service, external env file, lock, logs, health command, operator
  status command, and kill-switch/submission gates.

Known gaps:

- The `alpaca-index-credit-engine` binary is now a thin entrypoint over library account-engine
  code, but the account engine still needs a clean strategy-hosting abstraction. Additional
  strategies should plug into one account engine rather than becoming separate account-owning
  runners.
- The Python/core `OrderList` single-instrument constraint still blocks a simple Python-native MLeg
  strategy path. Keep the Rust `SubmitOrderList` MLeg path until a deliberate multi-instrument
  order-list abstraction is designed.
- Multi-day paper proof with real management closes is still outstanding.
- Websocket disconnect/reconnect and reconciliation events are wired, but they still need paper
  observation during an actual reconnect or broker event-loss scenario.
- Multi-account support currently has an operator/deployment foundation only. It still needs account
  admission policy, allocation rules, per-role strategy configs, and paper proof before undefined
  risk is allowed to submit.

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
  `alpaca-index-credit-engine` Rust runner complete; it scans, applies broker/state admission,
  selects one entry, and can submit through the Alpaca `SubmitOrderList` execution path when
  explicitly enabled.)
- Port scanner parameters for underlyings, DTE, width, delta, open interest, leg spread, minimum
  return-on-risk, and credit/debit-to-width floors. (Runner reads these from `ALPACA_CONFIG_PATH`,
  defaulting to `~/.config/nautilus-trader/alpaca/index-credit.toml`; ranking favors centered DTE
  and stronger minimum-leg open interest.)
- Add account/position/open-order admission checks. (Runner uses the Alpaca adapter admission gate.)
- Add deterministic sizing and daily duplicate-entry controls. (Runner uses TOML `index.quantity`
  and persists submitted entries by trade date and underlying.)
- Build and submit Nautilus `SubmitOrderList` entries directly. (Submission is disabled by default;
  set TOML `runtime.submit = true` or emergency override `ALPACA_SUBMIT=true` for paper execution.)
- Persist enough local strategy state to avoid duplicate submits after restart. (Default state path
  is `$XDG_STATE_HOME/nautilus_trader/alpaca_index_credit_state.json` or
  `$HOME/.local/state/nautilus_trader/alpaca_index_credit_state.json`.)
- Remove any dependency on `spreads` execution intents, workers, or Docker runtime.

Exit criteria:

- One paper run scans, selects, submits, observes order events, and leaves a coherent Nautilus state.

Status: initial native runner complete. Paper submit proof exists for Nautilus MLeg submission; keep
strategy submission disabled by default outside intentional paper tests.

Runner commands:

```bash
# Validate TOML/env config without scanning or submitting.
cargo run -p nautilus-alpaca --features live --bin alpaca-index-credit-engine -- --check-config

# Dry-run scan, no order submission.
ALPACA_IGNORE_ENTRY_WINDOW=true \
  cargo run -p nautilus-alpaca --features live --bin alpaca-index-credit-engine -- SPY,QQQ,IWM

# Paper submission path; use cancel-after-accept only for smoke testing.
ALPACA_SUBMIT=true \
ALPACA_CANCEL_AFTER_ACCEPT=true \
  cargo run -p nautilus-alpaca --features live --bin alpaca-index-credit-engine -- SPY,QQQ,IWM
```

Phase 7A call-credit mode:

```bash
# Scan both index put-credit and index call-credit candidates; submission still disabled.
ALPACA_IGNORE_ENTRY_WINDOW=true \
ALPACA_STRATEGIES=both \
  cargo run -p nautilus-alpaca --features live --bin alpaca-index-credit-engine -- SPY,QQQ,IWM
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
  expiration-risk exit. (Initial profit-target, stop-loss-debit, max-hold, stale-entry, debit and
  credit expiration-risk evaluation, and management snapshot telemetry complete.)
- Implement close MLeg order-list construction for verticals. (Initial reduce-only close
  `SubmitOrderList` construction complete for credit verticals.)
- Add cancel stale entry and close orders. (Runner can cancel stale entry orders and submit close
  orders only when TOML `runtime.manage = true` or `ALPACA_MANAGE=true`; close submission also
  requires TOML `runtime.close = true` or `ALPACA_CLOSE=true`.)
- Add manual flatten and global kill switch. (Initial `ALPACA_FORCE_FLATTEN` and
  `ALPACA_KILL_SWITCH` controls complete.)
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
ALPACA_MANAGE=false \
  cargo run -p nautilus-alpaca --features live --bin alpaca-index-credit-engine

# Allow stale-order cancel and close submission when triggers fire.
ALPACA_MANAGE=true \
ALPACA_CLOSE=true \
  cargo run -p nautilus-alpaca --features live --bin alpaca-index-credit-engine
```

Key controls:

- `ALPACA_KILL_SWITCH=true` blocks new entries.
- `ALPACA_FORCE_FLATTEN=true` treats every tracked open spread as a close candidate.
- TOML `management.stale_entry_secs = 900` cancels stale working entries when management is enabled.
- TOML `management.stale_close_secs = 120` cancels stale working close orders for reprice
  independently from stale entries.
- TOML `management.close_regular_hours_only = true` blocks non-forced close submissions outside
  regular option hours; forced flatten remains available for explicit operator action.
- TOML `management.close_price_cushion = 0.02` adds a small debit cushion to close limits.
- TOML `management.max_close_attempts = 3` caps accepted close submissions per entry.
- TOML `management.close_reprice_cooldown_secs = 30` delays close resubmission after an attempt.
- TOML `management.profit_target_close_fraction = 0.50` closes when debit falls to 50% of entry
  credit.
- TOML `management.stop_loss_close_multiple = 2.0` closes when debit reaches 2x entry credit.
- TOML `management.expiration_exit_days = 1` closes near expiration risk.

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
- The runner supports TOML `runtime.max_iterations = 0` for continuous service mode.

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
- Add Discord candidate alerts from the candidate ledger as a sidecar, scoped first to selected
  candidates, high-score candidates, and candidate submit rejections. Keep webhook delivery outside
  the trading loop so alert failures cannot block scanning, entry submission, or management.

Current status:

- `alpaca-index-credit-control.sh operator` reports service, account, orders, positions, strategy
  state, last scan, last decision, latest broker event, and alerts from one command.
- `alpaca-control alerts candidates` reads per-account candidate-ledger JSONL and can dry-run or send
  Discord candidate alerts using an external `alerts.env` webhook.
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

- Build and install release binaries for `alpaca-index-credit-engine` and
  `alpaca-operator-status`; stop using `cargo run` from systemd and control scripts. (Complete:
  installer builds release binaries and service/control scripts execute installed binaries.)
- Move strategy state structs, atomic state persistence, operator-event emission, and credit-spread
  management helpers from the runner binary into reusable `src/` modules. (State schema,
  persistence, and operator events moved to `runtime`; close-decision helpers moved to
  `management`.)
- Make the bin targets thin entrypoints over library code so tests can cover runtime decisions
  without shelling out to binaries. (Index-credit runner entrypoint complete; runtime state and
  management decisions have direct tests.)
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
- `alpaca-index-credit.service` was verified running `~/.local/bin/alpaca-index-credit-engine`
  instead of `cargo run`.
- `alpaca-index-credit-control.sh operator|health` was verified running
  `~/.local/bin/alpaca-operator-status`.
- Strategy state persistence is atomic and shared between runner and operator status through the
  `runtime` module.
- Credit-spread close decisions are shared through the `management` module with direct unit tests.
- Remaining Phase 6.5 work: paper-observe websocket reconnect/reconciliation events.

## Phase 6.6: Account Engine Abstraction

Goal: close the live-engine gap by making the Alpaca runtime a single account engine that hosts
strategies, not a one-off strategy runner.

Work:

- Split the remaining runner logic into library modules:
  - `runtime/config.rs`: environment/config parsing and validation. (Initial index-credit config
    moved to `index_credit`.)
  - `runtime/engine.rs`: account-engine loop, lifecycle, iteration cadence, shutdown behavior.
    (Initial index-credit account-engine loop moved to `index_credit_engine`.)
  - `runtime/selection.rs`: scan, admission, and candidate selection. (Initial index-credit
    selection moved to `index_credit`.)
  - `runtime/broker.rs`: submit, cancel, lookup, and reconciliation helper orchestration. (Initial
    index-credit submit/cancel/lookup orchestration moved to `index_credit_engine`.)
- Define an account context that owns broker connectivity, account snapshots, orders, positions,
  strategy state, operator events, and account-level risk controls. (Initial
  `AccountEngineContext` complete for hosted index-credit decisions.)
- Define a strategy runtime trait or equivalent interface so strategies produce decisions against
  the account context instead of directly owning broker I/O. (Initial `StrategyRuntime` trait
  complete.)
- Port index credit into that strategy interface while preserving current behavior and environment
  gates. (Complete for entry decisions; management still runs inside the account engine.)
- Keep Alpaca broker-native MLeg submission on the Rust `SubmitOrderList` path. Do not relax the
  Python/core `OrderList` single-instrument assumption just to support Alpaca option spreads.
- Represent strategy output as explicit decisions such as skip, submit open, submit close, cancel,
  force flatten, or alert. Broker submission remains account-engine responsibility. (Initial
  `StrategyDecision` covers skip, no-entry, dry-run, and submit-open.)
- Add unit tests for strategy decisions and account-engine orchestration without shelling out to
  binaries. (Initial credential-free entry-gate tests complete.)
- Keep the deployed service as one process for the active Alpaca account; new strategies are config
  entries inside that process.

Exit criteria:

- `bin/index_credit_engine.rs` is a thin entrypoint over library account-engine code.
- Index credit behavior is preserved through tests and dry-run/paper smoke commands.
- Adding another strategy does not require another account-owning systemd service.
- Strategy decisions can be tested without Alpaca credentials.
- The MLeg model boundary remains explicit and no global core/Python `OrderList` behavior is
  weakened.

Current status:

- `IndexCreditConfig` now lives in library code and owns environment parsing/validation for the
  index-credit runtime.
- Index-credit scan/admission/candidate selection now lives in library code as
  `select_index_credit_entry`.
- The installed `alpaca-index-credit-engine` binary is now a thin Tokio entrypoint over
  `index_credit_engine::run_index_credit_engine`.
- The account-engine loop, management orchestration, and broker submit/cancel/lookup helpers now
  live in library code.
- The engine hosts `IndexCreditStrategy` through `StrategyRuntime`, receives explicit
  `StrategyDecision` values, and keeps broker submission as account-engine responsibility.
- Direct tests cover underlying parsing, scanner-width parsing, and credential-free entry gates.
- Account-level risk caps are now part of the account engine and operator status:
  `max_active_entries`, `max_daily_submits`, and `max_open_orders` block new hosted-strategy
  submissions before any strategy can add exposure.
- Installed release binary was rebuilt and service health-checked after extraction on 2026-05-02.
- Remaining Phase 6.6 work: paper-observe the extracted hosted engine during market hours.

Status: engineering complete for index-credit account-engine hosting; market-hours paper proof
remains.

## Phase 6.7: Multi-Account Foundation

Goal: prepare the deployment for multiple Alpaca paper accounts without letting secondary accounts
trade before credentials, roles, permissions, and gates are reviewed.

Work:

- Add a read-only fleet registry with account ID, role, enabled flag, service name, env file, config
  file, log directory, lock directory, permissions, and risk budget.
- Add an account-scoped systemd service template so future accounts can run as separate supervised
  account engines with their own env, config, logs, locks, and strategy state.
- Add an account env template that defaults extra accounts to `ALPACA_KILL_SWITCH=true`,
  `ALPACA_SUBMIT=false`, `ALPACA_MANAGE=false`, and `ALPACA_CLOSE=false`.
- Add a fleet status command that checks enabled accounts by running the existing operator status
  command inside a hermetic child process built from each account's env file plus registry service
  and config metadata.
- Keep the current account as `paper-main` on `alpaca-index-credit.service`; keep future accounts
  disabled until credentials are provided and reviewed.

Exit criteria:

- The installed tooling can report the current account through fleet status.
- Disabled future account entries are visible in the registry but never started by install/control
  scripts.
- Account-scoped services do not share logs, locks, strategy state, or credentials.
- The architecture preserves role separation for defined-risk short premium, long-premium
  directional, and future undefined-risk/naked-call/naked-put accounts.

Status: initial implementation complete. The account engine reads the fleet registry on startup,
forces `submit=false` plus kill-switch when permissions do not match configured strategies, honors a
fleet-wide kill switch, applies fleet active-entry caps from account state paths, and surfaces fleet
policy blocks in operator status. This is not yet a full allocator.

## Phase 7: Strategy Migration

Migration order:

1. `index_put_credit_entry` (initial native runner complete)
2. `index_call_credit_entry` (Phase 7A scanner path complete through `ALPACA_STRATEGIES=call|both`)
3. `index_call_debit_entry` / `index_put_debit_entry` for the directional paper account (hosted
   scanner, submit, state, and debit-spread close-management path complete; paper proof pending)
4. `earnings_call_debit_entry` / `earnings_put_debit_entry` (debit-spread Alpaca MLeg order payload
   builders complete; explicit earnings-event CSV input policy complete; earnings-event selection
   remains parked)
5. Iron condors (native scanner, 4-leg open/close payload primitives, account-engine dry-run
   hosting, SPY dry-run proof complete, and strategy-level dry-run gating available while put/call
   credit spreads remain live; still needs paper submit/close proof)
6. Naked calls / naked puts for the undefined-risk paper account (native scanner, simple open/close
   order payloads, fleet permission gates, and state plumbing complete; paper proof pending)
7. Straddles/strangles after long-premium management rules are proven

Each strategy must have native open, management, close, reconciliation, paper validation, and
operator visibility before live enablement.

Phase 7 earnings input policy:

- Earnings events must come from an operator-approved local CSV with
  `underlying,report_date,timing,source` columns.
- `report_date` is exchange-local `YYYY-MM-DD`.
- `timing` must be `before_open`, `after_close`, or `unknown`; unknown timing is blocked by the
  default entry policy.
- The `earnings` module parses and filters events by entry window without Alpaca credentials.
- Do not infer earnings dates from broker option chains or snapshots.
- Alpha Vantage is the first approved low-cost upstream source. `earnings-sync` downloads
  `EARNINGS_CALENDAR`, caches the raw CSV for 23 hours by default, and writes normalized events to
  `$XDG_STATE_HOME/nautilus_trader/earnings/earnings_events.csv` or
  `$HOME/.local/state/nautilus_trader/earnings/earnings_events.csv`.
- `earnings-sync` also writes `earnings_events_approved.csv`, which applies the default
  strategy-safe filter: known timing only, future/default-window reports only, weekday reports only,
  and common listed-equity symbol shape only. Full raw normalized output remains available for
  diagnostics and manual review.
- `DebitSpreadScannerConfig`, `scan_call_debit_underlying`, `scan_put_debit_underlying`, and
  pure candidate builders now cover initial earnings-debit scanner mechanics.

Phase 7 blockers:

- Other strategy setup sequence:
  1. Keep existing paper positions under management before adding exposure.
  2. Enforce account-level caps before enabling additional strategies.
  3. Show cap status and cap alerts in operator status.
  4. Paper-prove caps with existing open strategy state.
  5. Enable `index_call_credit_entry` under strict caps.
  6. Paper-prove one call-credit open/management/close lifecycle. (Open proof complete on
     May 4, 2026; close proof remains under management.)
  7. Expand to put-plus-call only after single-call proof is clean. (Initial 10-symbol scan list is
     enabled under one-entry caps.)
  8. Enable iron condor scanning as dry-run only while put/call credit spreads remain live.
  9. Park earnings debit until explicitly resumed.
- Secondary Alpaca accounts should remain disabled until Phase 6.7 is installed, the user provides
  paper credentials, and each account receives a role-specific config and risk budget.
- Directional debit strategies now have account-engine hosting and management plumbing, but still
  need market-hours paper proof on `paper-directional` after current defined-risk exposure is flat or
  intentionally closed.
- Earnings debit strategies still need earnings-event selection policy integration, paper proof, and
  source-quality review of the Alpha Vantage feed before live use. Earnings work is intentionally
  skipped for the current scanner expansion.
- Iron condors now have native candidate construction, four-leg Alpaca MLeg open/close payload
  builders, hosted account-engine selection, dry-run decision emission, persisted four-leg state,
  four-leg close construction, and `runtime.dry_run_strategies` gating so iron condors can scan
  without submitting while other strategies submit. On 2026-05-03, a real Alpaca dry-run scan
  selected an SPY iron condor with submission disabled and the follow-up broker check showed zero
  positions and zero open orders. They still need management-rule review, submit-with-cancel paper
  proof, and full open/close paper proof before live enablement.
- Naked calls and naked puts now have native single-leg candidate scanning, Alpaca simple order open
  and close payloads, persisted single-leg state, fleet permission enforcement, and undefined-risk
  account config support. They still need real market-hours paper proof, assignment-risk review,
  buying-power/Greek budget enforcement beyond static fleet metadata, and emergency flatten proof.
- Straddles/strangles should wait until long-premium management and max-loss behavior are specified.

Parked next steps for earnings debit:

- Add earnings strategy config for approved CSV path, call/put debit enablement, entry window, max
  candidates, sizing, and submit gate. Submission remains disabled by default.
- Add dry-run earnings selection: read `earnings_events_approved.csv`, scan eligible call/put debit
  candidates, require Alpaca option-chain and snapshot liquidity, and emit explicit strategy
  decisions without broker submission.
- Host earnings debit in the single Alpaca account engine as another `StrategyRuntime`; do not add
  another account-owning service.
- Define long-premium management rules before any non-smoke paper open: max debit at entry,
  profit target, stop loss, time stop, expiration-risk handling, and manual flatten behavior.
  (Initial debit profit target, stop loss, time stop, expiration-risk, and manual flatten handling
  are wired; paper proof is still pending.)
- Validate in order: market-hours dry-run, submit-with-cancel smoke, paper open with defined close
  management, then multi-day soak.

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
- Phase 6.6 is complete for the index-credit account engine.
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

Paper-prove the undefined-risk paper account with strict caps, then verify one naked-option
open/management/close lifecycle before expanding the undefined-risk symbol list or sizing.
