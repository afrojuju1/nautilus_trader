# Nautilus Base, Alpaca Runtime, And Spreads Boundary Plan

This document records the current boundary between the Nautilus fork, the Alpaca options runtime,
and the separate `spreads` product system.

The Alpaca options runtime is Nautilus-owned. It runs as `alpaca-options-node`, uses Nautilus data
and execution clients, owns account-level admission and order submission, and persists operational
state in this repo's Postgres schema.

`spreads` can remain a product and operations surface: UI, APIs, discovery workflows, scheduler,
reporting, and operator views. It should not own Alpaca broker submission mechanics for strategy
families migrated into this runtime.

## Current Target

```text
spreads UI/API/CLI and product workflows
        |
        v
candidate and policy outputs, if integrated
        |
        v
alpaca-options-node
        |
        v
Nautilus data, risk, execution, cache, and Alpaca adapter clients
        |
        v
Alpaca broker APIs
```

The fork-local runtime is the order owner:

- `alpaca-options-node` owns process assembly, config loading, account preflight, runtime lease
  acquisition, and node lifecycle.
- `CandidateScanActor` owns Nautilus subscriptions and bounded scan orchestration.
- Family planners produce pure `EntryPlan` values.
- `AlpacaOptionsAccountStrategy` owns selected-plan arbitration, account-level risk gates, entry
  submission, close management, lifecycle blocks, and state projection.
- `AlpacaExecutionClient` is the only place that expands Nautilus orders into Alpaca broker payloads.

## Source Of Truth

| Fact | Owner |
| --- | --- |
| Live orders, positions, accounts, fills | Nautilus cache and execution state inside the running node |
| Operational strategy state | Postgres `strategy_state`, `strategy_state_account`, `strategy_broker_leg_evidence`, `strategy_state_events` |
| Candidate evidence and runtime outcomes | Postgres candidate and performance ledgers |
| Replay/backtest market data | `ParquetDataCatalog` |
| Analytical market-data acceleration | ClickHouse, behind explicit read-source flags |
| Product UI/API projections | `spreads`, rebuilt from deliberate exported facts if product integration is needed |

Postgres operational state is normalized. `strategy_state.entry_id` keys one account/strategy entry
row, preferring `order_list_id` and falling back to `spread_instrument_id` only for pre-replacement
rows without an order-list identity.

## Rules

1. Do not create another live trading loop outside Nautilus for Alpaca options.
2. Do not restore retired direct-submit binaries, standalone strategy loops, or external order
   handoff paths.
3. Do not let migrated strategy families silently fall back to direct Alpaca submission.
4. Do not put ClickHouse, Parquet, or research replay on the live order path.
5. Do not make the local JSON state file a primary source of truth; it is a recovery mirror only.
6. Do not keep permanent aliases for replaced runtime config or storage names.

## Spreads Integration Boundary

If `spreads` integrates with this fork again, the product system should produce strategy decisions,
policy limits, and operator-facing projections. The Nautilus runtime should still own final broker
validation, order construction, submission, reconciliation, and lifecycle events.

The integration contract should use Nautilus-owned domain objects wherever possible:

- Nautilus instruments and `InstrumentId` values.
- Nautilus `OptionSpread` identities for multi-leg spreads.
- Account-level risk and submission gates from `AlpacaOptionsAccountStrategy`.
- Broker facts sourced from Nautilus execution/cache and Alpaca reconciliation.
- Operational state read through the normalized Postgres projection.

## Current Work Items

- Keep `nt-ups.6` open for paper broker proof of native spread closes. The runtime close path now
  submits spread-backed exits as one reduce-only Nautilus `OptionSpread` order; non-spread entries
  block close submission until a native close path is designed.
- Keep `docs/developer_guide/alpaca_options_account_strategy_architecture.md` as the active runtime
  architecture.
- Keep `docs/developer_guide/alpaca_operational_state_runbook.md` as the operational-state migration
  and restore guide.
- Keep `docs/developer_guide/market_data_warehouse_workstream.md` as the Parquet/ClickHouse boundary
  guide.

## Validation

For Alpaca runtime changes:

```bash
cargo fmt -p nautilus-alpaca
cargo test -p nautilus-alpaca --features live --lib
cargo check -p nautilus-alpaca --features live --bins
cargo check -p nautilus-cli --features alpaca --bin nautilus
```

For runtime validation, use the Docker and operator commands in `AGENTS.md`, then report account,
orders, positions, operational-store status, and any alerts.
