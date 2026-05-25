# Alpaca TradingNode Gap Plan

This plan covers the first migration slice for regular Nautilus strategies that should run through
the standard Python `TradingNode` shape. The immediate target is `GapDownFragileRebound` on US
equity/ETF daily bars.

## Current State

- The Rust Alpaca runtime owns broker-paper options execution, account reconciliation, ledgers,
  management actions, and operator commands.
- Python now has a regular `GapDownFragileRebound` strategy which submits normal Nautilus
  `LimitOrder` objects for equity/ETF buys and sells.
- Python also has a regular `UpsideGapContinuation` strategy port for the next equity/ETF
  daily-bar package.
- The Python Alpaca data factory supports static US equity instruments and Alpaca stock-bar
  requests/polling.
- The Python Alpaca execution factory supports simple US equity/ETF `DAY` limit broker orders.
  The existing Rust `AlpacaExecutionClient` still owns the richer options execution runtime.

## Target Architecture

The final architecture should be one `TradingNode` per account/runtime profile, with one or more
strategies registered on that node when they share the same account, risk budget, and venue
connectivity.

```text
TradingNode
  |
  |-- AlpacaLiveDataClient
  |     |-- instruments
  |     |-- stock bars
  |     `-- later: option quotes/greeks
  |
  |-- AlpacaLiveExecClient
  |     |-- equity submit/cancel/query
  |     |-- REST reconciliation
  |     `-- later: trade updates and option/multi-leg bridge
  |
  `-- Strategies
        |-- GapDownFragileRebound
        |-- UpsideGapContinuation
        `-- later migrated strategies
```

Strategies should not call Alpaca directly. They should request data, observe portfolio/account
state, and submit Nautilus order commands. Adapter code should translate those commands into
Alpaca payloads and translate broker facts back into Nautilus execution reports.

## Implemented In This Slice

- Document the current and target architecture, including the remaining execution gap.
- Add static Alpaca equity instruments on venue `ALPACA`.
- Add minimal Python Alpaca stock-bar data support for externally aggregated bars.
- Add a `GapDownFragileRebound` Python node example using Alpaca data plus Nautilus sandbox
  execution.
- Add parity tests for the migrated GapDown signal gates.
- Add Python broker-paper execution for simple whole-share equity/ETF `DAY` limit orders.
- Add adapter-level shared equity risk gates for kill switch, max order notional, max total
  notional, buying power, duplicate symbol exposure, and short-sale blocking.
- Add a paper submit/cancel smoke harness for one tiny equity order.
- Add a profile loader so Python examples can use installed Rust runtime account env files.
- Add an `UpsideGapContinuation` Python strategy port and paper node example.

The default paper node path is intentionally broker-safe: it runs the regular strategy and submits
regular Nautilus orders, but the execution client is Nautilus sandbox execution. It is not a
strategy-level dry run. Passing `--broker-paper` deliberately routes those same Nautilus orders to
the Alpaca paper broker account.

## Remaining Gaps

1. Equity order lifecycle hardening.

   Validate Python `TradingNode` DAY limit buys/sells, client order IDs, fills, cancel flows, stale
   order handling, startup reconciliation, and periodic repair in Alpaca paper with tiny notional
   caps.

2. Shared account-level risk.

   The first adapter-level gates are in place for equities. The remaining work is to prove them in
   paper, decide policy defaults per account, add operator visibility, and promote any broader
   portfolio rules out of strategy-local code.

3. Option data and multi-leg Python execution.

   The Rust options runtime already handles much of this. Moving it under Python `TradingNode`
   requires exposing or wrapping the Rust `AlpacaExecutionClient`, explicit option quote
   subscriptions, snapshot/Greek refresh, multi-leg order-list support, and reconciliation for
   assignment, exercise, expiration, corrections, and fills.

4. Trade update stream parity.

   The Python execution client currently uses REST reconciliation. Trade update WebSocket handling
   should either move through the Rust bridge or be implemented with equivalent dedupe and repair
   behavior.

5. Operational projections.

   Candidate ledgers, performance summaries, and operator status views should remain projections
   over strategy decisions and broker facts. They should not become a second execution system.

## Migration Order After This Slice

1. Keep proving `GapDownFragileRebound` and `UpsideGapContinuation` against Alpaca broker-paper
   execution with tiny notional caps.
2. Prove the shared equity risk gates with broker-paper smoke tests and strategy runs.
3. Add a multi-strategy node example only after individual strategy runs have data, submit, fill,
   cancel, and reconciliation coverage.
4. Move options strategies into the same architecture after Python can represent the needed
   multi-leg execution lifecycle without losing Rust runtime safety.
