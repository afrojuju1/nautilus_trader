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
- `opportunity_scan_actor` adds a read-only `DataActor` that subscribes to option-chain slices,
  can bootstrap Alpaca option instruments through the standard data-client request path, produces
  `OptionsOpportunitySet`, stores the latest result in actor state, and emits structured operator
  events.
- `OptionsOpportunitySet` and `OptionsScanReport` now have narrow public constructors/mutators so
  REST-fed and Nautilus-fed scan paths share the same result shape.

## Immediate Follow-Ups

- [x] Add a node/example wiring `OptionChainOpportunityScanActor` into a real Nautilus node.
  - Implemented as `alpaca-option-chain-scan-node`, a `BacktestNode` executable over catalog
    `QuoteTick` and `OptionGreeks` data. This exercises Nautilus' native option-chain manager and
    actor lifecycle without submitting orders.
  - Run with:
    `cargo run -p nautilus-alpaca --features live,backtest-node --bin alpaca-option-chain-scan-node -- <CATALOG_PATH> <UNDERLYING> [VENUE]`.
- [x] Add Alpaca live data-client support for option `QuoteTick` and `OptionGreeks` subscriptions
  before wiring the actor into a live `TradingNode`.
  - Implemented through a shared Alpaca option snapshot poller in `AlpacaDataClient`.
  - `subscribe_quotes` emits Nautilus `QuoteTick`, `subscribe_option_greeks` emits
    `OptionGreeks`, and `request_forward_prices` bootstraps ATM-relative chains from stock
    snapshots.
  - Remaining live-data improvement: replace or augment REST polling with Alpaca option WebSocket
    streams when that path is added.
- [x] Add a REST-vs-option-chain comparison command for the same symbol, expiry, and scan time.
  - Implemented as `alpaca-compare-option-chain-scan`.
  - It loads one Alpaca REST option snapshot for the requested underlying/expiry, scans the same
    contracts through the legacy REST normalizer and the Nautilus `OptionChainSlice` normalizer,
    then emits JSON parity diagnostics without submitting orders.
- [x] Add a live Nautilus node for Alpaca option-chain candidate evidence and entry strategy wiring.
  - Implemented as `alpaca-option-chain-scan-live-node`.
  - The node registers `AlpacaDataClientFactory`, `AlpacaExecutionClientFactory`,
    `OptionChainOpportunityScanActor`, and `AlpacaOptionsEntryStrategy`.
  - The scan actor publishes typed `AlpacaOptionsOpportunityData`; the strategy subscribes through
    the Nautilus data bus and submits standard Nautilus orders only when the runtime submit/window
    gates allow it.
  - Until persisted strategy admission moves into the strategy path, entry submission also requires
    `ALPACA_OPTIONS_LIVE_ENTRY_SUBMIT_ENABLED=true`; otherwise the strategy remains wired but
    dry-runs selected entries.
- [ ] Decide whether scanner evidence should write to Postgres directly or publish events for a
  separate persistence consumer.
- [x] Remove the adapter-local order-plan layer from options-engine submission.
  - Submission now builds standard Nautilus `OrderAny` values through `OrderFactory`, then derives
    `SubmitOrder` or `SubmitOrderList` commands for `AlpacaExecutionClient`.
  - Alpaca-specific code remains at symbol normalization and execution-client payload translation;
    the account engine still owns broker session lifecycle and submit gates until entry admission
    moves behind a real strategy boundary.
- [x] Add a Nautilus-native entry strategy boundary.
  - `AlpacaOptionsEntryStrategy` owns conversion from `OptionsOpportunitySet` /
    `SelectedOptionsEntry` into standard Nautilus orders and submits through `Strategy::submit_order`
    or `Strategy::submit_order_list`.
  - The account-engine compatibility path reuses the same entry-order construction while the full
    `LiveNode` execution cutover is still in progress.
  - The account-engine compatibility path also reuses the strategy-owned entry order-list ID
    helper, leaving only state/reconciliation/admission responsibilities in the old loop.

## Cutover Readiness

The system is ready for side-by-side cutover proof, not for deleting the current account engine yet.
The old REST/account-engine path remains the order-capable runtime until the following checks are
green during market hours for the configured paper profiles.

- [x] REST-vs-option-chain scanner parity command exists.
  - Local: `deploy/alpaca/alpaca-control.sh compare-scan --pretty SPY 2026-07-02`
  - Docker: `ALPACA_COMPARE_UNDERLYING=SPY ALPACA_COMPARE_EXPIRY=2026-07-02 docker compose -f deploy/alpaca/compose.yml --profile cutover run --rm alpaca-compare-option-chain-scan`
- [x] Live Nautilus node exists with scanner-to-strategy wiring.
  - Local bounded run:
    `ALPACA_OPTION_CHAIN_MAX_RUNTIME_SECS=90 deploy/alpaca/alpaca-control.sh option-chain-live SPY 2026-07-02`
  - Docker bounded run:
    `ALPACA_OPTION_CHAIN_UNDERLYING=SPY ALPACA_OPTION_CHAIN_EXPIRY=2026-07-02 ALPACA_OPTION_CHAIN_MAX_RUNTIME_SECS=90 docker compose -f deploy/alpaca/compose.yml --profile cutover run --rm alpaca-option-chain-live`
- [x] One-command cutover proof exists.
  - `deploy/alpaca/alpaca-control.sh cutover-proof SPY 2026-07-02`
- [ ] Run the cutover proof during market hours across the paper-profile symbols and expiries.
  - Required signal: selected candidate and scan diagnostics match, or mismatches are explained by
    stricter Nautilus option-chain quote validity.
  - Required signal: live node logs show Alpaca instruments loaded, option-chain subscription with
    non-zero cached instruments, and `option_chain_opportunity_scan` events.
  - For undefined-risk profiles, set `ALPACA_OPTION_CHAIN_OPTIONS_BUYING_POWER` or allow the live
    node to read paper-account buying power before scanning.
  - Do not set `ALPACA_OPTIONS_LIVE_ENTRY_SUBMIT_ENABLED=true` for cutover proof until the
    persisted admission/state tasks below are complete.
- [ ] Move persisted entry admission and state recording into the Nautilus strategy path before
  retiring the account-engine entry loop.

## Next Loops To Retire

- [ ] Entry loop
  - Move persisted risk admission into `AlpacaOptionsEntryStrategy`: broker permission memory,
    active-entry limits, daily submit limits, open-order limits, per-underlying/sector limits, and
    broker account/position/open-order admission checks.
  - Record accepted/rejected submission state from execution reports or reconciled order status so
    restarts do not lose duplicate-entry protection.
  - Retire direct account-engine entry submission after the strategy owns both admission and state
    recording.

- [ ] Management loop
  - Move close, flatten, stale-order, and reprice lifecycle into a dedicated strategy/component.
  - Keep broker reconciliation and lifecycle decisions observable through operator events.

- [ ] One-off scanner binaries
  - Retire long-running standalone scanner loops after actor/strategy paths provide equivalent
    evidence.
  - Keep only intentionally diagnostic commands with clear no-order semantics.

## Guardrails

- Do not add generic scanner wrappers.
- Do not preserve duplicate runtime paths once a Nautilus-native path replaces them.
- Keep candidate math pure, deterministic, and independent of Alpaca REST types.
- Keep order-capable changes behind explicit strategy/risk boundaries.
