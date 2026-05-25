# Alpaca TradingNode Gap Plan

This plan covers the first migration slice for regular Nautilus strategies that should run through
the standard Python `TradingNode` shape. The immediate target is `GapDownFragileRebound` on US
equity/ETF daily bars.

## Current State

- The Rust Alpaca runtime owns broker-paper options execution, account reconciliation, ledgers,
  management actions, and operator commands.
- Python now has a regular `GapDownFragileRebound` strategy which submits normal Nautilus
  `LimitOrder` objects for equity/ETF buys and sells.
- The Python Alpaca data factory supports static US equity instruments and Alpaca stock-bar
  requests/polling.
- Python broker execution through `AlpacaLiveExecClientFactory` is still not wired. The existing
  Rust `AlpacaExecutionClient` is not exposed as a Python `LiveExecutionClient`.

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
  |     |-- submit/cancel/modify
  |     |-- trade updates
  |     `-- REST reconciliation
  |
  `-- Strategies
        |-- GapDownFragileRebound
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

The paper node path is intentionally broker-safe: it runs the regular strategy and submits regular
Nautilus orders, but the execution client is Nautilus sandbox execution. It is not a strategy-level
dry run.

## Remaining Gaps

1. Python Alpaca execution bridge.

   Expose or wrap the Rust `AlpacaExecutionClient` as a Python `LiveExecutionClient`, or implement
   an equivalent Python client that generates normalized order, fill, cancel, and position reports.
   This must include startup reconciliation and periodic repair before it can replace sandbox
   execution.

2. Equity order lifecycle parity.

   The Rust Alpaca order payload path supports simple equity buy/sell limit payloads, but the
   Python node cannot yet route orders to that Rust client. Once the execution bridge exists,
   validate DAY limit buys/sells, client order IDs, fills, cancel flows, and stale order handling in
   Alpaca paper.

3. Shared account-level risk.

   Multiple regular strategies on one account need shared admission controls. The node should own
   account-wide exposure, buying power, duplicate symbol, and kill-switch checks so strategies
   cannot bypass each other.

4. Option data and multi-leg Python execution.

   The Rust options runtime already handles much of this. Moving it under Python `TradingNode`
   requires explicit option quote subscriptions, snapshot/Greek refresh, multi-leg order-list
   support, and reconciliation for assignment, exercise, expiration, corrections, and fills.

5. Operational projections.

   Candidate ledgers, performance summaries, and operator status views should remain projections
   over strategy decisions and broker facts. They should not become a second execution system.

## Migration Order After This Slice

1. Build the Python execution bridge for Alpaca equities, starting with paper DAY limit buy/sell.
2. Run `GapDownFragileRebound` against Alpaca broker-paper execution with tiny notional caps.
3. Add account-wide risk controls for multiple regular equity strategies.
4. Port the next strategy only after the first strategy has data, submit, fill, cancel, and
   reconciliation coverage.
5. Move options strategies into the same architecture after Python can represent the needed
   multi-leg execution lifecycle without losing Rust runtime safety.
