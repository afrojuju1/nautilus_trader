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
- As of June 22, 2026, Rust also has an `AlpacaDataClient` and `AlpacaDataClientFactory` that can
  exact-load Alpaca option instruments through the Nautilus `DataClient` request/subscription
  surface.

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
- Add a combined equity daily strategy example with both migrated strategies sharing one Alpaca
  data client, execution client, and account-level risk gate surface.

The default paper node path is intentionally broker-safe: it runs the regular strategy and submits
regular Nautilus orders, but the execution client is Nautilus sandbox execution. It is not a
strategy-level dry run. Passing `--broker-paper` deliberately routes those same Nautilus orders to
the Alpaca paper broker account.

## Remaining Gaps

1. Equity order lifecycle hardening.

   Validate Python `TradingNode` DAY limit buys/sells, client order IDs, fills, cancel flows, stale
   order handling, startup reconciliation, and periodic repair in Alpaca paper with tiny notional
   caps. A closed-market broker-paper run on May 25, 2026 proved connection, account load, bar
   subscription, historical bar receipt, and graceful shutdown for the combined equity node; natural
   signal/order observation still needs the next open market window.

2. Shared account-level risk.

   The first adapter-level gates are in place for equities. The remaining work is to prove them in
   paper, decide policy defaults per account, add operator visibility, and promote any broader
   portfolio rules out of strategy-local code.

3. Option data and multi-leg Python execution.

   The Rust options runtime already handles much of this, and the Rust data-client foundation now
   exact-loads option instruments through Nautilus `DataClient`. Moving the full path under Python
   `TradingNode` still requires explicit option quote subscriptions, snapshot/Greek refresh,
   multi-leg order-list support, execution-client exposure or wrapping, and reconciliation for
   assignment, exercise, expiration, corrections, and fills. The first target is the put-credit
   spread path because Rust already has option contract loading, snapshots, scanner scoring,
   multi-leg order-list construction, Alpaca `mleg` payload validation, and paper submit/cancel
   harnesses. See [Alpaca Options TradingNode Migration](alpaca_options_trading_node_migration.md).

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
3. Prove the combined equity daily strategy node through a longer paper session and capture broker
   submit/cancel/fill/reconciliation behavior under tiny caps.
4. Use the Rust `AlpacaDataClient` as the standard option-instrument bridge, then move the
   put-credit spread path into the same architecture after the node can represent option snapshots,
   `SubmitOrderList` commands, multi-leg broker submission, cancel/close flows, option positions,
   and account reconciliation without losing Rust runtime safety.
