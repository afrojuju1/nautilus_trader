# Alpaca Production Complete Plan

This document is the high-level plan for making this fork the production trading engine for the
Alpaca option-spread workflow. The target state is Nautilus-native execution and strategy runtime.
`spreads` is reference material only and must not remain in the live trading loop.

Public documentation readiness is tracked in
[Alpaca Official Docs Readiness Checklist](alpaca_official_docs_readiness_checklist.md). The
current public-facing integration page is [Alpaca](../integrations/alpaca.md), which documents the
implemented Rust options runtime as experimental until the standard node path covers option data,
multi-leg execution, and operator lifecycle parity.

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
- Alpaca has a native Rust `DataClient` and factory for exact option-instrument loading through the
  standard Nautilus request/subscription interface.
- Alpaca execution client supports option-spread `SubmitOrderList`; single-order submit is denied
  for now.
- Trade-update websocket handling maps Alpaca parent and leg updates back to Nautilus leg client
  order IDs.
- Paper smoke tests have submitted an MLeg spread through Nautilus, observed accepted leg events,
  canceled the parent, and verified zero positions/open orders afterward.
- The fork has a documented upstream sync workflow in `AGENTS.md`.
- Native `put_credit` and `call_credit` scanner/entry paths run from the
  Rust Alpaca runner and submit through Nautilus `SubmitOrderList` when explicitly enabled.
- Initial credit/debit spread management can cancel stale entries, evaluate close triggers with
  expiration-risk exits, emit management snapshots with PnL context, build reduce-only close MLegs,
  and mark filled closes in strategy state.
- The NUC has a supervised user service, external env file, lock, logs, health command, operator
  status command, and kill-switch/submission gates.

Known gaps:

- The `alpaca-options-node` binary is now a thin entrypoint over library account-engine
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
- Assignment, exercise, and expiration lifecycle risk is now polled from Alpaca account activities
  and blocks entry admission when configured. Remaining lifecycle work is paper observation and
  performance-ledger attribution.
- The scanner still relies primarily on REST snapshots. Active-risk option quote freshness uses
  explicit Nautilus quote subscriptions for active entries and high-rank candidates. The Alpaca
  data client now prefers the option market-data WebSocket stream for quotes/trades and keeps REST
  snapshots as quote fallback and Greeks transport.
- Historical opportunity tracking exists, but strategy tuning still needs a replay/research harness
  over historical option data and candidate ledgers before sizing or strategy expansion is justified
  by evidence.

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
- Poll option account activities for assignment, expiry, and option trade settlement records and
  translate them into explicit operator/runtime events.
- Add account capability preflight for options approval/trading levels so strategies above the
  account's current level are blocked before scanner or broker submission.
- Add cancel/replace support for working MLeg entries where Alpaca supports it.
- Keep single-leg submit disabled unless a strategy explicitly needs it.

Exit criteria:

- A restarted Nautilus process can reconstruct account, orders, fills, and positions without
  `spreads`.
- Paper tests cover accepted, rejected, canceled, filled, and startup-reconciled MLeg orders.

Status: initial production-capable slice complete; continue validating edge broker lifecycle events
as paper/live proof expands.

## Phase 3: Native `put_credit`

Goal: run the first strategy fully inside Nautilus.

Work:

- Build a Nautilus-native put-credit strategy or strategy runner. (Initial
  `alpaca-options-node` Rust runner complete; it scans, applies broker/state admission,
  selects one entry, and can submit through the Alpaca `SubmitOrderList` execution path when
  explicitly enabled.)
- Port scanner parameters for underlyings, DTE, width, delta, open interest, leg spread, minimum
  return-on-risk, and credit/debit-to-width floors. (Runner reads these from `ALPACA_CONFIG_PATH`,
  defaulting to `~/.config/nautilus-trader/alpaca/options.toml`; ranking favors centered DTE
  and stronger minimum-leg open interest.)
- Add account/position/open-order admission checks. (Runner uses the Alpaca adapter admission gate.)
- Add deterministic sizing and daily duplicate-entry controls. (Runner uses TOML `index.quantity`
  and persists submitted entries by trade date and underlying.)
- Build and submit Nautilus `SubmitOrderList` entries directly. (Submission is disabled by default;
  set TOML `runtime.submit = true` or emergency override `ALPACA_SUBMIT=true` for paper execution.)
- Persist enough local strategy state to avoid duplicate submits after restart. (Default state path
  is `$XDG_STATE_HOME/nautilus_trader/alpaca_options_state.json` or
  `$HOME/.local/state/nautilus_trader/alpaca_options_state.json`.)
- Remove any dependency on `spreads` execution intents, workers, or Docker runtime.

Exit criteria:

- One paper run scans, selects, submits, observes order events, and leaves a coherent Nautilus state.

Status: initial native runner complete. Paper submit proof exists for Nautilus MLeg submission; keep
strategy submission disabled by default outside intentional paper tests.

Runner commands:

```bash
# Validate TOML/env config without scanning or submitting.
cargo run -p nautilus-alpaca --features live --bin alpaca-options-node -- --check-config

# Dry-run scan, no order submission.
ALPACA_IGNORE_ENTRY_WINDOW=true \
  cargo run -p nautilus-alpaca --features live --bin alpaca-options-node -- SPY,QQQ,IWM

# Paper submission path; use cancel-after-accept only for smoke testing.
ALPACA_SUBMIT=true \
ALPACA_CANCEL_AFTER_ACCEPT=true \
  cargo run -p nautilus-alpaca --features live --bin alpaca-options-node -- SPY,QQQ,IWM
```

Phase 7A call-credit mode:

```bash
# Scan both index put-credit and index call-credit candidates; submission still disabled.
ALPACA_IGNORE_ENTRY_WINDOW=true \
ALPACA_STRATEGY_FAMILIES=both \
  cargo run -p nautilus-alpaca --features live --bin alpaca-options-node -- SPY,QQQ,IWM
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
- Add expiration/assignment risk automation: DTE-zero entry block timing, close-before-expiry
  controls, ITM/buying-power checks where data allows, and operator alerts for positions exposed to
  exercise or assignment risk.
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
  cargo run -p nautilus-alpaca --features live --bin alpaca-options-node

# Allow stale-order cancel and close submission when triggers fire.
ALPACA_MANAGE=true \
ALPACA_CLOSE=true \
  cargo run -p nautilus-alpaca --features live --bin alpaca-options-node
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
- Store env/secrets outside the repo. (Initial `deploy/alpaca/alpaca-options.env.example`
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
- On 2026-05-02, `alpaca-options.service` was installed, started, health-checked, and stopped
  on `ade-nucbox-k8-plus` with kill-switch enabled and submission disabled.
- The runner supports TOML `runtime.max_iterations = 0` for continuous service mode.

Exit criteria:

- The NUC can start, stop, restart, and report status for the Nautilus engine cleanly.

Status: complete for supervised NUC proof. Production hardening remains in Phase 6.5.

## Phase 6: Operator Visibility And Alerts

Goal: operate the engine without reading raw logs as the primary interface.

Work:

- Add CLI/status output for account, orders, positions, strategy state, last scan, last decision, and
  last broker event. (Initial `alpaca-ops status` binary complete with human and JSON output.)
- Add alerts for rejected orders, websocket disconnect, stale working orders, unmanaged positions,
  reconciliation mismatches, kill-switch activation, and service restart. (Initial alert summary is
  implemented in `alpaca-ops status`; websocket disconnect alerts consume structured disconnect
  events when the live engine emits them.)
- Add structured event logs for strategy decisions and broker state transitions. (Runner emits
  structured `operator_event=` JSON for start, iteration, decision, and submit result events.)
- Add Discord candidate alerts from the candidate ledger as a sidecar, scoped first to selected
  candidates, high-score candidates, and candidate submit rejections. Keep webhook delivery outside
  the trading loop so alert failures cannot block scanning, entry submission, or management.

Current status:

- `alpaca-control.sh operator` reports service, account, orders, positions, strategy
  state, last scan, last decision, latest broker event, and alerts from one command.
- `alpaca-control alerts candidates` consumes typed `candidate_alert` records from per-account
  candidate-ledger JSONL and can dry-run or send Discord candidate alerts using an external
  `alerts.env` webhook.
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

- Build and install release binaries for `alpaca-options-node` and
  `alpaca-ops status`; stop using `cargo run` from systemd and control scripts. (Complete:
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
- `alpaca-options.service` was verified running `~/.local/bin/alpaca-options-node`
  instead of `cargo run`.
- `alpaca-control.sh operator|health` was verified running
  `~/.local/bin/alpaca-ops status`.
- Strategy state persistence is atomic and shared between runner and operator status through the
  `runtime` module.
- Credit-spread close decisions are shared through the `management` module with direct unit tests.
- Remaining Phase 6.5 work: paper-observe websocket reconnect/reconciliation events.

## Phase 6.6: Account Engine Abstraction

Goal: close the live-engine gap by making the Alpaca runtime a single account engine that hosts
strategies, not a one-off strategy runner.

Work:

- Split the remaining runner logic into library modules:
  - `runtime/config.rs`: environment/config parsing and validation. (Initial options-runtime config
    moved to `options_runtime`.)
  - `runtime/engine.rs`: account-engine loop, lifecycle, iteration cadence, shutdown behavior.
    (Initial options-runtime account-engine loop moved to `options_engine`.)
  - `runtime/selection.rs`: scan, admission, and candidate selection. (Initial options-runtime
    selection moved to `options_runtime`.)
  - `runtime/broker.rs`: submit, cancel, lookup, and reconciliation helper orchestration. (Initial
    options-runtime submit/cancel/lookup orchestration moved to `options_engine`.)
- Define an account context that owns broker connectivity, account snapshots, orders, positions,
  strategy state, operator events, and account-level risk controls. (Initial
  `AccountEngineContext` complete for hosted options-runtime decisions.)
- Define a strategy runtime trait or equivalent interface so strategies produce decisions against
  the account context instead of directly owning broker I/O. (Initial `StrategyRuntime` trait
  complete.)
- Port options into that strategy interface while preserving current behavior and environment
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

- `bin/options_engine.rs` is a thin entrypoint over library account-engine code.
- Index credit behavior is preserved through tests and dry-run/paper smoke commands.
- Adding another strategy does not require another account-owning systemd service.
- Strategy decisions can be tested without Alpaca credentials.
- The MLeg model boundary remains explicit and no global core/Python `OrderList` behavior is
  weakened.

Current status:

- `AlpacaOptionsRuntimeConfig` now lives in library code and owns environment parsing/validation for the
  options-runtime runtime.
- Options opportunity discovery now lives in library code as `scan_options_candidates`, which
  returns an explicit candidate set for hosted strategies to evaluate.
- The installed `alpaca-options-node` binary is now a thin Tokio entrypoint over
  `options_engine::run_options_engine`.
- The account-engine loop, management orchestration, and broker submit/cancel/lookup helpers now
  live in library code.
- The engine hosts `OptionsRuntimeStrategy` through `StrategyRuntime`, receives explicit
  `StrategyDecision` values, and keeps broker submission as account-engine responsibility.
- Direct tests cover underlying parsing, scanner-width parsing, and credential-free entry gates.
- Account-level risk caps are now part of the account engine and operator status:
  `max_active_entries`, `max_daily_submits`, and `max_open_orders` block new hosted-strategy
  submissions before any strategy can add exposure.
- Installed release binary was rebuilt and service health-checked after extraction on 2026-05-02.
- Remaining Phase 6.6 work: paper-observe the extracted hosted engine during market hours.

Status: engineering complete for options-runtime account-engine hosting; market-hours paper proof
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
- Keep the current account as `paper-main` on `alpaca-options.service`; keep future accounts
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

1. `put_credit` (initial native runner complete)
2. `call_credit` (Phase 7A scanner path complete through `ALPACA_STRATEGY_FAMILIES=call|both`)
3. `call_debit` / `put_debit` for the directional paper account (hosted
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
  5. Enable `call_credit` under strict caps.
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
  four-leg close construction, and `runtime.dry_run_families` gating so iron condors can scan
  without submitting while other strategies submit. On 2026-05-03, a real Alpaca dry-run scan
  selected an SPY iron condor with submission disabled and the follow-up broker check showed zero
  positions and zero open orders. They still need management-rule review, submit-with-cancel paper
  proof, and full open/close paper proof before live enablement.
- Naked calls and naked puts now have native single-leg candidate scanning, Alpaca simple order open
  and close payloads, persisted single-leg state, fleet permission enforcement, and undefined-risk
  account config support. They still need real market-hours paper proof, assignment-risk review,
  buying-power/Greek budget enforcement beyond static fleet metadata, and emergency flatten proof.
- Straddles/strangles should wait until long-premium management and max-loss behavior are specified.

## Phase 7.5: Alpaca Capability Roadmap

Goal: refine the live Alpaca runtime with the minimum platform features needed for official-quality
documentation and evidence-driven strategy operation.

Required before promoting beyond experimental:

1. Account capability preflight. **Status: runtime implementation complete.**
   - Read account options approval/trading-level fields and account configuration limits at startup
     and in operator status.
   - Map enabled strategies to the required options level and fail closed when an account is not
     approved for the configured strategy set.
   - Emit clear operator blocks such as `options_level_insufficient` before scanning or submitting.
   - Implemented through `account_capabilities`, `AlpacaHttpClient::account_configuration`, and
     `alpaca-options-node` live-submit readiness.

2. Assignment, exercise, and expiration risk daemon. **Status: runtime implementation complete.**
   - Poll account activities for option assignment, exercise, and expiry records because these
     lifecycle events are not guaranteed through trade-update websockets.
   - Add DTE-zero and near-expiry controls aligned with Alpaca's expiration handling window.
   - Surface exercise/assignment/expiry events in operator status and alerts.
   - Keep manual DNE and exercise instructions as documented operator procedures unless an explicit
     API-backed workflow is designed and paper-proven.
   - Implemented through `options_lifecycle`, the node-level account-activity poller, and
     `AlpacaOptionsStrategy` lifecycle entry blocks. Performance-ledger attribution remains
     reporting work, not an order-path safety blocker.

3. Active-risk option quote cache. **Status: runtime implementation complete on the current
   Nautilus quote subscription path.**
   - `AlpacaOptionsStrategy` subscribes explicit active-entry option legs and top-ranked candidate
     option legs through Nautilus `subscribe_quotes`.
   - `AlpacaDataClient` satisfies those subscriptions with Alpaca option market-data WebSocket
     quotes when available, falls back to REST snapshots when the stream is unavailable, and emits
     `QuoteTick` values using Alpaca event timestamps when present.
   - Snapshot quote fallback resumes after two snapshot poll intervals without a stream quote, so
     a connected-but-silent stream does not freeze the active-risk quote cache.
   - Management close marks and close triggers read the Nautilus quote cache first. Stale cached
     active-entry quotes block close attempts, emit `active_risk_quote_stale`, and surface in
     `alpaca-ops status`.
   - Candidate entry checks block on stale cached selected-leg quotes, but do not block first-time
     entries solely because the subscription cache has not emitted yet.
   - `alpaca-ops status` includes the latest `option_market_data_stream` event so operators can
     distinguish stream-backed freshness from snapshot fallback.

4. Historical option research and replay harness. **Status: initial read-only harness complete.**
   - `alpaca-ops replay` replays candidate ledgers against Alpaca historical option bars where
     available.
   - Produces score-bucket, strategy, underlying, DTE, delta, spread-width, and liquidity summaries
     with evaluated and missing-mark counts.
   - Uses bar-close research marks only; it is not an execution-quality fill simulator.
   - See [Alpaca Historical Option Replay](alpaca_historical_replay.md) for the data contract and
     validation commands.
   - Remaining research improvement: add richer historical Greek and option-chain evidence when
     those datasets are available in the warehouse.

High-value after the required platform work:

5. Strategy regime router.
   - Status: v1 feature input contract is defined; implementation intentionally waits for pure
     router types and a real read-only feature actor. Do not add fake `neutral` regime metadata to
     ledgers just to create a router-shaped surface.
   - Build this as a Nautilus-native routing layer, not a standalone scanner. A read-only regime
     feature actor derives features from bars, option-chain state, external signals, and approved
     historical feature sources; a pure regime router returns labels, confidence, explanation codes,
     and strategy-family weights.
   - Route strategy families, not individual orders. Risk admission remains the final gate, and the
     router must not submit orders, mutate strategy state, or call Alpaca directly.
   - Record regime label, confidence, feature freshness, feature version, routing action, and
     explanation codes in the candidate ledger for every scan.
   - Use the focused [Alpaca Regime Router](alpaca_regime_router.md) design for v1 labels, approved
     feature sources, freshness rules, confidence semantics, storage/evidence boundaries, failure
     policy, validation ranges, and rollout slices.

6. Portfolio Greek and stress governor.
   - Status: initial risk-capital stress governor complete in the entry-admission path. It stores
     per-entry `risk_capital_usd`, derives legacy defined-risk/debit/naked-put estimates where
     possible, blocks single-entry and projected portfolio risk-capital excess, and fails closed on
     unknown active exposure when configured. Greek aggregation remains dependent on live Greek
     feature/storage inputs.
   - Track account and fleet delta, gamma, vega, theta, and buying-power usage from active option
     exposure where data allows.
   - Add scenario stress such as underlying +/-1%, +/-2%, volatility up/down, and gap-open
     approximations before allowing new exposure.
   - Make this a higher-level risk layer above count caps so the system can block correlated
     exposures that look harmless by entry count alone.

7. Fill-quality intelligence.
   - Status: initial reporting complete in `alpaca-ops performance`. Performance rows now report
     quoted-vs-actual entry cashflow, entry slippage, fill delay, fill completeness, and explicit
     missing inputs; summary and by-strategy aggregates include fill-quality metrics.
   - Measure each submitted order against quote midpoint, bid/ask spread, quote age, fill delay,
     reprice count, and post-fill drift.
   - Summarize fill quality by strategy, underlying, account, time of day, and order type.
   - Feed poor execution quality back into universe scoring, scanner thresholds, and symbol
     quarantine.

8. Smart MLeg repricing engine.
   - Status: initial close-side bounded repricing complete on the existing management path.
     `close_reprice_step` and `max_close_price_cushion` build an attempt-aware close price ladder
     using persisted `close_attempts`, existing stale-close cancel, max-attempt, and cooldown
     gates. Entry-side replacement remains blocked by current duplicate-entry safety policy until a
     deliberate entry-replacement model is designed.
   - Submit entries and closes with a controlled limit ladder instead of one static price.
   - Start near the desired midpoint, improve by configured ticks while edge remains acceptable,
     and cancel when quote quality or expected value decays.
   - Keep repricing bounded by max attempts, max slippage, stale-quote checks, and current
     management gates.

9. Event shock guard.
   - Status: initial earnings event-shock admission block complete. The guard reads an approved
     earnings CSV when configured, can fail startup if event data is required but missing, and
     records `event_shock_earnings` as a first-class decision reason. News and corporate-action
     sources remain future input contracts.
   - Consume earnings, real-time news, historical news, and corporate-action inputs to pause symbols
     around material events.
   - Add symbol cooldowns for mergers, splits, lawsuit/FDA/headline shocks, unexpected halts, and
     event clusters.
   - Record event blocks as first-class decision reasons so skipped trades can be reviewed.

10. Replay lab and decision time machine.
    - Status: initial decision explanations complete in candidate outcomes and `alpaca-ops replay`.
      Selected-candidate reason/details are preserved, replay records include decision reasons and
      broker rejection reasons, and replay reports bucket outcomes by `by_decision_reason`.
    - Reconstruct any trading day from config, account state, market snapshots, candidate ledgers,
      decisions, submissions, fills, and management snapshots.
    - Produce a human-readable explanation for why a trade was selected, blocked, submitted,
      repriced, closed, or skipped.
    - Use the replay output for docs, audits, and regression tests when scanner/risk rules change.

11. Native roll and adjustment engine.
   - Support explicit spread/condor roll candidates that close current legs and open replacement
     legs as one broker-native MLeg order where Alpaca supports the structure.
   - Keep rolls disabled by default until open, replace/cancel, close, and rollback behavior have
     paper proof.

Parked for now:

- Equity-plus-option combo orders, because Alpaca currently documents restrictions around equity
  legs in MLeg orders.
- Broker API multi-user product work; the current target remains this fork's trading engine, not a
  broker platform.
- API-triggered exercise instructions, unless we intentionally design a separate operator workflow.

Reference sources:

- Alpaca Options Trading: `https://docs.alpaca.markets/docs/options-trading`
- Alpaca Options Level 3 Trading: `https://docs.alpaca.markets/docs/options-level-3-trading`
- Alpaca Real-time Option Data: `https://docs.alpaca.markets/docs/real-time-option-data`
- Alpaca Historical Option Data: `https://docs.alpaca.markets/docs/historical-option-data`
- Alpaca Real-time News: `https://docs.alpaca.markets/docs/streaming-real-time-news`
- Alpaca Historical News Data: `https://docs.alpaca.markets/v1.3/docs/historical-news-data`
- Alpaca Corporate Actions: `https://docs.alpaca.markets/reference/corporateactions-1`
- Alpaca Market Clock: `https://docs.alpaca.markets/reference/clock-1`
- Alpaca Market Calendar: `https://docs.alpaca.markets/reference/calendar-2`

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
- Phase 6.6 is complete for the options-runtime account engine.
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

Implement the Phase 7.5 required platform work in this order:

1. Account capability preflight. Done in the runtime path.
2. Assignment, exercise, and expiration risk daemon. Done for runtime gating and operator status.
3. Active-risk option quote cache on the current Nautilus subscription path. Done for runtime
   gating and operator status; option quote/trade streaming is wired with snapshot fallback and
   still needs market-hours entitlement validation.
4. Historical option research and replay harness. Initial read-only `alpaca-ops replay` command is
   done for historical option-bar marks and aggregate summaries; richer historical Greek and
   option-chain evidence depends on warehouse availability.
5. Portfolio risk-capital stress governor. Initial implementation complete; richer Greek/stress
   inputs depend on feature storage.
6. Fill-quality intelligence, close-side MLeg repricing, event shock guard, and replay decision
   explanations. Initial implementations complete on existing runtime/reporting paths.
7. Strategy regime router. Still open by design until pure router types and a real feature actor
   exist.

In parallel with that platform work, continue paper-proving the undefined-risk account under strict
caps. Do not expand undefined-risk symbols or sizing until one naked-option open/management/close
lifecycle and the new lifecycle-risk checks are proven.
