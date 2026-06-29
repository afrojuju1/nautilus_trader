# Alpaca Options TradingNode Migration

This document tracks the migration away from direct Alpaca REST strategy utilities and toward normal
Nautilus `TradingNode` data and execution flows.

## Current Checkpoint

The first standard Python node slice is implemented.

- `AlpacaLiveDataClientFactory` creates an Alpaca data client for static US equity instruments,
  exact OCC option instruments, stock bars, and option snapshot quotes/Greeks.
- `AlpacaLiveExecClientFactory` creates an Alpaca execution client for simple US equity/ETF `DAY`
  limit orders and two-to-four-leg option `SubmitOrderList` commands.
- The options multi-leg Python example registers exact option instruments, subscribes option
  snapshot quotes/Greeks, and can submit/cancel a paper multi-leg order through the standard
  execution client when explicitly confirmed.
- The Rust `alpaca-options-node` remains the account-level options runtime for hosted strategy
  families, risk gates, lifecycle handling, operator projections, and paper/live operational proof.

The direct Rust MLeg payload validation/submission diagnostics have been retired. New smoke proof
should exercise either the Python `TradingNode` path or the Rust `alpaca-options-node` runtime, not
standalone Alpaca payload posting.

## Target Architecture

Strategies should be regular Nautilus strategies running inside a `TradingNode`.

```text
TradingNode
  |
  |-- Alpaca data client
  |     |-- exact option instruments
  |     |-- option snapshot quotes/Greeks
  |     |-- stock bars
  |     `-- underlying equity context
  |
  |-- Alpaca execution client
  |     |-- SubmitOrderList -> Alpaca mleg submit
  |     |-- parent and leg order status mapping
  |     |-- cancel/close/reconcile
  |     `-- account, position, activity reports
  |
  `-- Nautilus strategy
        |-- consumes data from the node/cache
        |-- emits normal Nautilus commands
        `-- does not call Alpaca REST directly
```

The important boundary is that scanner and candidate-selection logic may remain Rust-backed, but
broker I/O sits behind the adapter. Strategies consume node data and submit Nautilus commands; they
do not own Alpaca credentials, REST clients, broker payloads, or reconciliation.

## Remaining Gaps

1. Strategy ownership for the exported Python put-credit scanner surface.

   `AlpacaPutCreditStrategy` is still a timer scaffold over PyO3 scanner bindings. It does not yet
   consume `TradingNode` option data or emit `SubmitOrderList` commands. Track this separately from
   adapter plumbing so the public Python strategy surface either becomes real Nautilus strategy code
   or leaves the public adapter API.

2. Trade-update streaming in the Python execution client.

   The Python client currently relies on REST reconciliation. The Rust runtime owns trade-update
   WebSocket handling today. Promote streaming only when it plugs into the standard execution client
   event path without adding a parallel broker loop.

3. Option market-data streaming in the Python data client.

   The Python node path polls option snapshots. Native option quote/trade streaming should improve
   the Alpaca data client and strategy cache path directly.

4. Full lifecycle proof for assignment, exercise, expiration, and correction events.

   The Rust runtime has lifecycle handling and activity polling; broader Python-node parity should
   reuse the same adapter/account report semantics rather than introducing new lifecycle daemons.

5. Historical strategy evaluation through standard stores.

   Historical candidate replay and performance analytics belong in `nautilus adapters alpaca replay` /
   `nautilus adapters alpaca performance` backed by the operational store and market-data catalog/warehouse. Do
   not grow adapter-owned custom backtest binaries.

## Done Criteria For The Migration

- Public examples use the Python `TradingNode` or `alpaca-options-node` as the execution proof path.
- Strategy code does not post directly to Alpaca REST.
- Option data enters strategies through Nautilus data/cache contracts.
- Option orders leave strategies as normal Nautilus commands.
- Operator ledgers and reports remain projections over strategy decisions and broker facts.
- Historical evaluation uses the maintained replay/performance read models, not a parallel
  adapter-owned backtest engine.
