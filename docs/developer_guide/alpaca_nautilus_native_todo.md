# Alpaca Nautilus-Native TODO

This is the working checklist for slimming the Alpaca options runtime toward Nautilus-native actors
and strategies.

## Current Workstream

- [x] Option-chain input adapter
  - Convert `OptionChainSlice` snapshots into `CandidateContract` and
    `CandidateMarketSnapshot` inputs.
  - Preserve scanner diagnostics and rejection counts from the shared candidate engine.
  - Compare REST-fed and Nautilus-fed candidates before changing order-capable behavior.

- [x] Read-only scan actor
  - Subscribe to Nautilus option-chain slices through `DataActor::subscribe_option_chain`.
  - Rank candidates through the shared candidate engine.
  - Emit operator evidence without submitting, closing, or managing orders.
  - Keep database ledger persistence as an explicit follow-up unless the actor runtime has an async
    storage boundary.

## Implemented Checkpoint

- `option_chain_candidates` converts Nautilus `OptionChainSlice` snapshots into the normalized
  candidate-engine model and ranks credit spreads, debit spreads, iron condors, and naked options.
- `candidate_scan_actor` adds a read-only `DataActor` that subscribes to option-chain slices,
  can bootstrap Alpaca option instruments through the standard data-client request path, produces
  `OptionsCandidateSet`, stores the latest result in actor state, and emits structured operator
  events.
- `OptionsCandidateSet` and `OptionsScanReport` now have narrow public constructors/mutators so
  REST-fed and Nautilus-fed scan paths share the same result shape.

## Immediate Follow-Ups

- [x] Add a node/example wiring `OptionChainCandidateScanActor` into a real Nautilus node.
  - Implemented as `alpaca-option-chain-scan-node`, a `BacktestNode` executable over catalog
    `QuoteTick` and `OptionGreeks` data. This exercises Nautilus' native option-chain manager and
    actor lifecycle without submitting orders.
  - Run with:
    `cargo run -p nautilus-alpaca --features live,backtest-node --bin alpaca-option-chain-scan-node -- <CATALOG_PATH> <UNDERLYING> [VENUE]`.
- [x] Add Alpaca live data-client support for option `QuoteTick` and `OptionGreeks` subscriptions
  before wiring the actor into a live `TradingNode`.
  - Implemented through `AlpacaDataClient` using Nautilus-native subscriptions.
  - `subscribe_quotes` emits Nautilus `QuoteTick`, `subscribe_option_greeks` emits
    `OptionGreeks`, and `request_forward_prices` bootstraps ATM-relative chains from stock
    snapshots.
  - Option quote subscriptions prefer Alpaca's option market-data WebSocket `v1beta1/{feed}` stream
    using MsgPack frames, and keep the REST snapshot poller as quote fallback when the stream is
    unavailable or returns entitlement/protocol errors.
  - Snapshot quote fallback also resumes when no stream quote has arrived for two snapshot poll
    intervals, so a connected-but-silent stream does not freeze active-risk freshness.
  - Option trade subscriptions use the standard Nautilus `subscribe_trades` path and emit
    `TradeTick` values with `AggressorSide::NoAggressor` because Alpaca's stream does not provide
    aggressor side.
  - `OptionGreeks` remain snapshot-backed because the option stream does not carry Greeks.
  - Standard external-LAST `RequestBars` for Alpaca option instruments now map to Alpaca historical
    option bars through `AlpacaDataClient`; unsupported bar aggregations return an empty
    `BarsResponse` after logging the request error.
  - `nautilus adapters alpaca status` surfaces the latest `option_market_data_stream` event so operators can see
    stream vs snapshot-fallback source and feed.
- [x] Add a REST-vs-option-chain comparison command for the same symbol, expiry, and scan time.
  - Implemented as `alpaca-compare-option-chain-scan`.
  - It loads one Alpaca REST option snapshot for the requested underlying/expiry, scans the same
    contracts through the REST snapshot normalizer and the Nautilus `OptionChainSlice` normalizer,
    then emits JSON parity diagnostics without submitting orders.
- [x] Add a live Nautilus node for Alpaca option-chain candidate evidence and entry strategy wiring.
  - Implemented as `alpaca-options-node`.
  - The node registers `AlpacaDataClientFactory`, `AlpacaExecutionClientFactory`,
    `OptionChainCandidateScanActor`, and `AlpacaOptionsAccountStrategy`.
  - The scan actor publishes typed `OptionsCandidateData`; the strategy subscribes through
    the Nautilus data bus and submits standard Nautilus orders only when the runtime submit/window
    gates allow it.
  - Entry admission now runs in the strategy path. Live entry submission uses the normal runtime
    `ALPACA_OPEN_ORDERS=true` gate; otherwise the strategy remains wired but dry-runs selected
    entries.
  - Do not enable live entry submission until durable strategy-state persistence, startup
    reconciliation, and remaining broker-account admission parity are complete.
- [x] Wire scanner and strategy evidence through a persistence sink/consumer after durable
  strategy-state writes are in place; do not write Postgres directly from synchronous strategy
  callbacks.
- [x] Remove the adapter-local order-plan layer from options-runtime submission.
  - Submission now builds standard Nautilus `OrderAny` values through `OrderFactory`; spread entries
    and spread-backed closes use single `OptionSpread` orders expanded by `AlpacaExecutionClient`.
  - Alpaca-specific code remains at symbol normalization and execution-client payload translation;
    the account engine still owns broker session lifecycle and submit gates until entry admission
    moves behind a real strategy boundary.
- [x] Add a Nautilus-native entry strategy boundary.
  - `AlpacaOptionsAccountStrategy` owns conversion from `OptionsCandidateSet` /
    `SelectedOptionsEntry` into standard Nautilus orders and submits through `Strategy::submit_order`.
  - `alpaca-options-node` now owns entry submission, management lifecycle, and `--check-config`.
  - Close, stale-order, force-flatten, and reprice decisions are owned by the same strategy state
    owner that records accepted entries.

## Cutover Readiness

The live Nautilus node is now the order-capable runtime. Remaining cutover proof is market-hours
validation and operational cleanup, not keeping a second account-engine owner alive.

- [x] REST-vs-option-chain scanner parity command exists.
  - Local:
    `cargo run -p nautilus-alpaca --features live --bin alpaca-compare-option-chain-scan -- --pretty SPY 2026-07-02`
  - Docker: `ALPACA_COMPARE_UNDERLYING=SPY ALPACA_COMPARE_EXPIRY=2026-07-02 docker compose -f deploy/alpaca/compose.yml --profile cutover run --rm alpaca-compare-option-chain-scan`
- [x] Live Nautilus node exists with scanner-to-strategy wiring.
  - Local bounded run:
    `ALPACA_OPTION_CHAIN_MAX_RUNTIME_SECS=90 cargo run -p nautilus-alpaca --features live --bin alpaca-options-node -- SPY 2026-07-02`
  - Docker bounded run:
    `ALPACA_OPTION_CHAIN_UNDERLYING=SPY ALPACA_OPTION_CHAIN_EXPIRY=2026-07-02 ALPACA_OPTION_CHAIN_MAX_RUNTIME_SECS=90 docker compose -f deploy/alpaca/compose.yml --profile cutover run --rm alpaca-option-chain-live`
- [x] One-command cutover proof retired from `alpaca-control`.
  - Keep proof steps explicit so the deploy wrapper does not own migration diagnostics.
- [ ] Run the cutover proof during market hours across the paper-profile symbols and expiries.
  - Required signal: selected candidate and scan diagnostics match, or mismatches are explained by
    stricter Nautilus option-chain quote validity.
  - Required signal: live node logs show Alpaca instruments loaded, option-chain subscription with
    non-zero cached instruments, and `option_chain_candidate_scan` events.
  - For undefined-risk profiles, set `ALPACA_OPTION_CHAIN_OPTIONS_BUYING_POWER` or allow the live
    node to read paper-account buying power before scanning.
  - Keep `ALPACA_OPEN_ORDERS=false` for cutover proof until the persisted admission/state tasks below
    are complete.
- [x] Finish durable strategy-state recording and startup reconciliation in the Nautilus strategy
  path before retiring the account-engine entry loop.

## Remaining Work Breakdown

### 1. Market-Hours Cutover Proof

- [ ] Run explicit compare and bounded node proof commands during market hours for the configured
  paper profiles and representative symbols/expiries.
- [ ] Confirm REST-vs-option-chain selected candidates and scan diagnostics match, or document
  mismatches caused by stricter Nautilus option-chain quote validity.
- [ ] Confirm live-node logs show Alpaca instrument bootstrap, non-zero cached option instruments,
  option-chain subscription, and `option_chain_candidate_scan` events.
- [ ] Keep `ALPACA_OPEN_ORDERS=false` for market-hours cutover proof. Strategy admission, durable state
  recording, and startup reconciliation are code-complete, but live paper submission still needs a
  bounded broker proof.

### 2. Entry Admission Cutover

Target: `AlpacaOptionsAccountStrategy` owns entry admission before any order is submitted. The old
account-engine entry owner has been removed; remaining work is market-hours proof that the strategy
path refuses entries using the same durable state and broker constraints.

- [x] Move the reusable admission model out of `options_engine.rs` into a strategy-owned module.
  - [x] Move `SubmissionBlock`, admission-block reason classification, broker-permission memory, and
    risk-gate decision conversion together.
  - [x] Keep reason strings stable so operator events, alerts, and ledgers remain comparable during
    cutover.
- [x] Feed admission with durable strategy state.
  - [x] Load `StrategyState` for the live node before constructing `AlpacaOptionsAccountStrategy`.
  - [x] Require storage-backed state when `ALPACA_OPEN_ORDERS=true`; dry-run
    cutover proof can keep the current no-storage live-node path.
  - [x] Preserve same-day duplicate checks, active-entry limits, daily-submit limits, per-underlying
    limits, per-sector limits, and fleet limits.
- [x] Feed admission with live broker state.
  - [x] Prefer Nautilus cache/portfolio/order state where it exposes equivalent information.
  - [x] Keep Alpaca HTTP only for account flags or option-specific broker admission details not already
    represented by Nautilus runtime state.
  - [x] Preserve the existing account/position/open-order option-spread admission checks before paper
    submission is enabled.
  - Startup live-submit readiness reads Alpaca HTTP account, positions, and open orders, reconciles
    strategy state, and blocks unmanaged broker state. Per-candidate admission uses Nautilus cache
    positions plus open/inflight orders.
- [x] Make the strategy emit the same selected/blocked/dry-run operator evidence as the old loop.
  - [x] Selected dry-runs must record `submission_disabled`.
  - [x] Blocks must include reason, current, limit, and details where available.
  - [x] The cutover proof showed decision parity before the old entry loop was removed.
- [ ] Done when `AlpacaOptionsAccountStrategy` refuses every entry the account engine would have
  refused, using the same persisted state and live broker constraints.

### 3. Entry State Recording

Target: accepted and terminally rejected entry submissions are recorded from the Nautilus strategy
path, so restarts and duplicate-entry protection do not depend on the old account-engine submit
loop.

- [ ] Add a strategy-owned pending submission record keyed by order-list ID.
  - [x] Store selected entry, trade date, quantity, expected client order IDs, and order count.
  - [x] Store submit timestamps.
  - [x] Keep this state in memory immediately after `submit_order` / `submit_order_list`.
- [x] Record accepted submissions from Nautilus order events.
  - [x] Use `Strategy::on_order_accepted`, `on_order_rejected`, and `on_order_denied`.
  - [x] Record `entry.state_entry_draft(...)` once a submission has meaningful broker acceptance.
  - [x] Preserve parent/venue order ID data when available from events or follow-up broker lookup.
- [x] Preserve terminal rejection behavior.
  - [x] Naked-option uncovered-permission rejections must still mark a canceled state entry with
    `entry_rejected_uncovered_option_permission`.
  - [x] Other terminal entry rejections should remain observable and should not create false active
    exposure.
- [x] Add a real persistence boundary for strategy state.
  - Follow `docs/developer_guide/operational_postgres_plan.md`.
  - [x] Enable `sqlx` Postgres migrations for operational Postgres storage.
  - [x] Move inline operational schema DDL into versioned SQL migration files.
  - [x] Add `strategy_state_events` as the append-only state mutation ledger.
  - [x] Add `version`, writer metadata, and last-event metadata to `strategy_state`.
  - [x] Add a transactional repository method that inserts one state event and updates the
    snapshot idempotently.
  - [x] Add a live-submit runtime lease or equivalent DB-visible exclusive writer guard.
  - Strategy callbacks are synchronous, while the Postgres repository is async; do not hide DB
    writes inside blocking callback code.
  - Use an explicit state persistence actor/event sink or another Nautilus-native async boundary
    for DB writes.
  - Keep local/in-memory state updated before persistence completes so admission gates protect the
    current process immediately.
  - [x] Mark the sink unhealthy, deny new entries, and emit operator evidence when persistence
    fails.
- [x] Reconcile state on startup before enabling paper submission.
  - [x] Load persisted entries.
  - [x] Require migrations, operational readiness, healthy sink, and an active account writer lease
    when `ALPACA_OPEN_ORDERS=true`.
  - [x] Reconcile broker orders/positions so pending or partially accepted entries are not double
    submitted after restart.
- [x] Done when accepted/rejected strategy submissions update durable state without calling direct
  account-engine submission code.

### 4. Paper Submit Cutover

- [ ] Enable `ALPACA_OPEN_ORDERS=true` only for a bounded paper run after entry admission, durable state
  recording, and startup reconciliation are strategy-owned.
- [ ] Confirm submitted orders flow through `AlpacaExecutionClientFactory`, standard Nautilus order
  events, and durable strategy state.
- [ ] Check broker account/orders after the run and cancel accepted smoke orders unless explicitly
  leaving them open.
- [x] Keep account capability preflight in the live-submit readiness path before paper submission.
  - `alpaca-options-node` checks options approval/trading level, account configuration max options
    level, account configuration trading blocks, and options buying power.
- [x] Keep lifecycle risk blocks in the strategy admission path before paper submission.
  - The node polls option assignment, exercise, and expiration account activities.
  - Recent assignment/exercise activity and same-day-expiry candidates block live entry submission.
  - `nautilus adapters alpaca status` surfaces the latest lifecycle poll and raises lifecycle alerts.

### 5. Remove Old Entry Loop

- [x] Delete direct account-engine entry submission after the strategy path owns admission and
  durable state.
- [x] Remove displaced entry-only helpers from `options_engine.rs`; keep only reconciliation
  behavior that still has an explicit owner.
- [x] Remove `ALPACA_OPTIONS_LIVE_ENTRY_SUBMIT_ENABLED`; the official order-capable path now uses
  the normal runtime `ALPACA_OPEN_ORDERS` gate only.

### 6. Scanner Evidence Persistence

- [x] Persist scanner evidence through the scan actor's async candidate-ledger sink.
- [x] Keep scan math and candidate ranking pure; persistence consumes scan results and does not own
  candidate selection.

### 7. Management Loop Cutover

- [x] Move close, flatten, stale-order, and reprice lifecycle into a Nautilus-owned
  strategy/component.
  - `AlpacaOptionsAccountStrategy` owns active-entry management on a Nautilus timer, subscribes to
    close-leg quotes, claims persisted active-entry instruments for reconciliation, and submits
    close orders through `Strategy::submit_order` / `Strategy::submit_order_list`.
- [x] Keep broker reconciliation, close decisions, and operator events visible during cutover.
  - Startup broker reconciliation remains in the live-node readiness path.
  - Management emits `management_snapshot` and `management_block` events from the strategy path.
- [x] Keep active-risk option quotes in the Nautilus strategy/cache path.
  - `AlpacaOptionsAccountStrategy` subscribes active-entry close legs and top-ranked candidate legs through
    `subscribe_quotes`.
  - Cached `QuoteTick` timestamps drive stale-quote close blocks, selected-candidate freshness
    blocks when a stale cached quote exists, and `nautilus adapters alpaca status` quote-cache/stale alerts.
  - The Alpaca data client keeps broad option-chain scan quotes on snapshot refresh, while active
    management and top-candidate quote interests consume a bounded option-stream budget.
  - Missing active close-leg quotes remain a `close_quote_missing` management block; first-time
    candidate entries are not blocked solely because the subscription cache has not emitted yet.
- [x] Remove direct management behavior from the account-engine loop once the new owner is proven.
  - The retired `options_engine` library module was removed.
  - `alpaca-options-node` no longer runs a management loop and remains a config-check command.
  - Docker/systemd runner defaults now start `alpaca-options-node`.

### 8. Diagnostic Cleanup

- [x] Retire long-running standalone scanner loops after actor/strategy paths provide equivalent
  evidence.
- [x] Remove intentionally diagnostic cutover commands from `alpaca-control`.
  - Scan comparison remains a direct diagnostic binary and Docker profile, not a durable control
    command.

## Next Loops To Retire

- [x] Entry loop
  - Tracked in "2. Entry Admission Cutover", "3. Entry State Recording", and
    "5. Remove Old Entry Loop" above.

- [x] Management loop
  - Tracked in "7. Management Loop Cutover" above.

- [x] One-off scanner binaries
  - Tracked in "8. Diagnostic Cleanup" above.

## Guardrails

- Do not add generic scanner wrappers.
- Do not preserve duplicate runtime paths once a Nautilus-native path replaces them.
- Keep candidate math pure, deterministic, and independent of Alpaca REST types.
- Keep order-capable changes behind explicit strategy/risk boundaries.
