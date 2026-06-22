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
  produces `OptionsOpportunitySet`, stores the latest result in actor state, and emits structured
  operator events.
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
- [ ] Add a REST-vs-option-chain comparison command for the same symbol, expiry, and scan time.
- [ ] Decide whether read-only actor evidence should write to Postgres directly or publish events
  for a separate persistence consumer.

## Next Loops To Retire

- [ ] Entry loop
  - Move entry selection, risk admission, and order-intent construction into a Nautilus `Strategy`.
  - Keep the strategy fed by `OptionsOpportunitySet` or its successor, not Alpaca REST payloads.
  - Submit through standard Nautilus order flow only.

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
