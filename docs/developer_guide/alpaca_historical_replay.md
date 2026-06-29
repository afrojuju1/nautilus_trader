# Alpaca Historical Option Replay

This workstream evaluates recorded Alpaca option candidates against historical option market data.
It is research-only: it does not submit, cancel, reconcile, or mutate strategy state.

This is the maintained historical evaluation path. Do not restore adapter-owned custom backtest
binaries for strategy scoring; use `nautilus adapters alpaca replay` / `nautilus adapters alpaca performance` over the
operational store and market-data catalog/warehouse, or promote reusable backtest behavior into the
standard Nautilus backtest/catalog architecture.

Run it through the operator CLI:

```bash
nautilus adapters alpaca replay --since 2026-06-01 --until 2026-06-05 --max-rank 3 --max-candidates 100
```

Use `--json` for machine-readable output and `--include-records` when per-candidate rows are needed.

## Data Contract

Dataset: `alpaca_historical_option_replay`

Owner: Alpaca options runtime in this fork.

Sources:

- Postgres `candidate_ledger` records for the configured storage account.
- Alpaca historical option bars from `AlpacaHttpClient::option_bars`.

Availability and entitlement:

- Alpaca historical option bars require option market-data entitlements for the configured feed.
- Alpaca's historical option data availability begins in February 2024; earlier request ranges can
  return empty or partial bar sets and should be treated as missing historical evidence.
- Standard Nautilus `RequestBars` now uses the same Alpaca option-bar endpoint through
  `AlpacaDataClient` for external LAST minute/hour/day option bars.

Consumers:

- Strategy threshold review.
- Candidate score-bucket, strategy, underlying, DTE, delta, spread-width, and liquidity analysis.
- Future scanner and regime-router tuning.

Time semantics:

- `trade_date` is the candidate-ledger trading date.
- Candidate event time is `candidate_ledger.payload.ts_utc`.
- Replay mark time is the latest available option bar at or before
  `ts_utc + --lookahead-minutes`, but not before `ts_utc`.

Primary record key:

- `trade_date`
- `candidate_identity_key`
- `lookahead_minutes`
- `timeframe`

Required candidate fields:

- `type = "candidate"`
- `trade_date`
- `ts_utc`
- `strategy`
- `underlying`
- option leg symbols
- `credit` for credit entries or `debit` for debit entries

Output summary fields:

- aggregate record counts, selected/submitted/rejected/virtual counts
- evaluated and missing mark counts
- wins, losses, flats, hypothetical PnL, average win/loss, largest loss
- total and average option-bar volume
- bucketed summaries by strategy, underlying, DTE, score, delta, spread width, and liquidity

## Interpretation

Historical replay uses bar close prices as a research mark. It is not an execution-quality fill
simulator and should not be used as proof that a live close would have filled at that price.
The candidate outcome tracker uses the same historical option-bar mark semantics only as a fallback
when current snapshots cannot value an older candidate. Those persisted outcome rows are marked with
`mark_source = historical_bar`.

Credit entries compute replay PnL as `entry_credit - close_mark`. Debit entries compute replay PnL
as `close_mark - entry_debit`. PnL is multiplied by the standard option contract multiplier and the
candidate quantity.

Liquidity buckets are based on the total historical option-bar volume across legs used for the mark.
Missing bars are reported separately and excluded from win/loss/flats.

Candidate-outcome reports can contain multiple observation buckets for the same candidate. Use
[Alpaca Candidate Outcome Analytics](alpaca_candidate_outcome_analytics.md) for aggregation rules
before comparing strategy families or tuning thresholds.

## Validation

Build and smoke-check the command with:

```bash
cargo check -p nautilus-alpaca --features live --bins
nautilus adapters alpaca replay --help
```

Run against real data only with configured storage and Alpaca data credentials:

```bash
nautilus adapters alpaca replay --json --since YYYY-MM-DD --until YYYY-MM-DD --max-rank 3 --max-candidates 100
```
