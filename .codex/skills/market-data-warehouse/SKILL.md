---
name: market-data-warehouse
description: Guide Nautilus market-data warehouse and storage-boundary work. Use when planning or implementing ClickHouse, catalog backfill, dual-write market-data ingestion, flagged read cutover, Postgres operational storage boundaries, warehouse migrations, or cleanup that must stay repo-level and source-neutral rather than Alpaca-specific.
---

# Market Data Warehouse

## Overview

Keep market-data warehouse work source-neutral and aligned with Nautilus ownership. ClickHouse is a
repo-level analytical backend; Alpaca may be the first proof source, but it must not own warehouse
schema, deployment, migration, or read-cutover contracts.

## Start Here

Read only the docs needed for the task:

- For ClickHouse, catalog backfill, dual-write, read-source flags, or warehouse deployment, read
  `docs/developer_guide/market_data_warehouse_workstream.md`.
- For Postgres operational state, strategy-state events, sqlx migrations, runtime leases, or
  slimming Postgres, read `docs/developer_guide/operational_postgres_plan.md`.
- For candidate outcome reporting grain, mark source, or Postgres-versus-ClickHouse reporting
  boundaries, read `docs/developer_guide/alpaca_candidate_outcome_analytics.md`.

## Architecture Rules

- Keep `ParquetDataCatalog` as the replay/backtest-compatible store.
- Use ClickHouse for high-volume analytical market data, feature series, range queries, and
  flagged read cutover.
- Keep Postgres as the slim operational control plane: strategy state snapshots, state events,
  candidate/performance/outcome ledgers, runtime leases, and small ingest manifests.
- Do not store bulk quotes, trades, bars, Greeks, or market-data-shaped caches in Postgres.
- Do not make ClickHouse the source of truth for strategy state, broker evidence, or realized PnL.
- Do not build Alpaca-owned warehouse modules, migrations, or deployment files. Put warehouse code
  in repo-level ownership such as `nautilus-persistence`, `schema/sql/clickhouse/`, and
  `deploy/warehouse/`.
- Do not add `shadow_compare` or ongoing comparison loops. Add explicit operator validation
  commands for requested dataset/range checks, then cut over reads by flag.
- Use `sqlx` migrations for Postgres operational storage. Use the warehouse operator plus the
  ClickHouse Rust client for ClickHouse DDL until a real need for heavier tooling exists.
- Treat ClickHouse write failures as warehouse lag by default, not as a live-trading stop, unless a
  strategy explicitly requires fresh warehouse-derived features.

## Implementation Order

Default to this order unless the user gives a narrower target:

1. Confirm the target bead or create/update one for durable work.
2. Check the relevant design doc and current code ownership before editing.
3. Implement the smallest source-neutral slice.
4. Validate through project CLIs, range-bounded warehouse checks, or targeted compile checks.
5. Update docs and Beads with the exact validation performed.

For the initial ClickHouse lane, prefer:

- Self-hosted `deploy/warehouse/` service and migrations.
- Generic `nautilus-persistence` ClickHouse boundary.
- Catalog-backed quote backfill proof.
- Live dual-write after backfill and resource checks.
- Explicit validation command.
- Flagged read cutover.

## Validation

Report the concrete validation, not generic confidence. Use `git diff --check` for docs/config
changes. Use targeted Rust checks for changed crates. Use live ClickHouse/Postgres smoke commands
only when services are intentionally running or the user asks to roll them.
