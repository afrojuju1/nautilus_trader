# Alpaca Options TradingNode Migration

This is the first concrete options migration slice after the equity `TradingNode` work.

## Current Checkpoint

On May 25, 2026, the Alpaca paper market clock was closed. Alpaca reported the next regular
session open as May 26, 2026 at 09:30 ET, so a natural equity signal/order lifecycle cannot be
observed until that session.

The combined equity node was still exercised through the live broker-paper path with the
`paper-directional` profile. It connected to Alpaca paper, loaded account state, initialized zero
open orders and zero positions, subscribed all configured equity bars, received historical bars,
and shut down cleanly through `TradingNode.stop()`.

The same profile also verified the current option surfaces:

- Account status is active and not trading-blocked.
- Account state reported zero open positions and zero open orders.
- The Rust Alpaca option loader returned `1650` active SPY put instruments for the near expiration
  window.
- The Rust snapshot loader requested `100` option snapshots and received `67` quotes with Greeks or
  IV.
- The Rust multi-leg payload builder produced a valid Alpaca `mleg` put-credit payload.

## First Migration Target

Start with the Alpaca put-credit spread path.

This is the right first target because it already has working pieces in the Rust Alpaca adapter:

- Option contract loading and conversion into Nautilus `OptionContract` values.
- Option snapshot loading with quote, IV, and Greek fields.
- Put-credit candidate scoring.
- Multi-leg order-list construction.
- Alpaca `mleg` payload validation.
- Paper submit/cancel harnesses.
- Strategy-state, candidate, outcome, and performance ledgers in the runtime.

This should not start with the `spreads_notebook` short-DTE long-call package. That research did not
clear the packaging bar. It also should not start with `options_gap_core_v1` as an option-order
strategy: that package is an equity strategy with an options-confirmation data filter, so it belongs
after the base equity book and option data surface are stable.

## Target Architecture

The strategy should become a regular Nautilus strategy running inside a `TradingNode`.

```text
TradingNode
  |
  |-- Alpaca data client
  |     |-- option contracts
  |     |-- option quotes/snapshots/Greeks
  |     `-- underlying equity context
  |
  |-- Alpaca execution client
  |     |-- SubmitOrderList -> Alpaca mleg submit
  |     |-- parent and leg order status mapping
  |     |-- cancel/close/reconcile
  |     `-- option account, position, activity reports
  |
  `-- PutCreditSpread strategy
        |-- consumes data from the node
        |-- emits normal Nautilus order-list commands
        `-- does not call Alpaca REST directly
```

The important boundary is that scanner logic may remain Rust, but broker I/O should sit behind the
adapter. Strategies should consume node data and submit Nautilus commands; they should not own
Alpaca credentials, REST clients, broker payloads, or reconciliation.

## Concrete Broker Gaps

1. Option instrument loading in the Python `TradingNode` adapter.

   Rust can parse Alpaca contracts into Nautilus `OptionContract` values, but the Python Alpaca live
   data factory currently uses `AlpacaEquityInstrumentProvider`. The node needs an Alpaca option
   provider that can load contracts by underlying, expiration window, option type, and explicit OCC
   symbols, then publish those instruments into the cache before strategies submit option orders.

2. Option data requests and subscriptions.

   The Python Alpaca data client currently supports static equity instruments and stock bars. The
   options strategy needs option snapshots or quote ticks with bid, ask, IV, Greeks, open interest,
   and timestamps. The first slice can poll snapshots; streaming quote ownership can remain a later
   decision.

3. Strategy-to-order-list construction.

   Rust already has `build_mleg_submit_order_list` and normalized option leg plans. The Python
   strategy path needs an exposed builder or equivalent order factory helper so a selected put-credit
   candidate becomes a normal Nautilus `SubmitOrderList` with stable client order IDs and linked
   legs.

4. Multi-leg execution submission.

   The Python Alpaca execution client currently denies `SubmitOrderList` as unsupported. The next
   implementation must translate two-to-four option legs into Alpaca `order_class=mleg`, submit the
   parent order, and emit accepted/rejected events for the Nautilus order-list legs without inventing
   a second broker path.

5. Cancel and close lifecycle.

   The adapter needs parent-order cancellation by venue order ID or client order ID, close-order-list
   construction for reduce-only legs, and idempotent handling for already-terminal parent orders.

6. Option position and account reports.

   The equity client maps account, equity positions, open orders, fills, and activities. The options
   path needs equivalent report generation for option positions, nested multi-leg orders, option
   account activities, fills, assignment, exercise, expiration, and correction events.

7. Options risk gates.

   Equity notional caps are not enough. Defined-risk spreads need max loss per spread, max contracts,
   max open risk, per-underlying limits, daily submission limits, options buying power checks,
   duplicate exposure checks, kill switch handling, and permission/rejection classification.

8. Operator projections.

   Candidate ledgers, strategy state, outcome tracking, and performance reporting should remain
   projections over strategy decisions and broker facts. They should not be a separate execution
   system once the `TradingNode` broker path is active.

## Implementation Order

1. Add an Alpaca option instrument provider and a small paper-profile option contract check under
   the Python adapter surface.
2. Add option snapshot polling to the Python Alpaca data client, backed by the existing Rust REST
   model where practical.
3. Expose the Rust option-leg plan/order-list builder to Python or add an equivalent tested Python
   builder that produces canonical Nautilus `SubmitOrderList` commands.
4. Implement Alpaca `SubmitOrderList` handling in the execution client for two-to-four option legs.
5. Add paper submit/cancel tests and a one-contract paper smoke harness with tiny risk caps.
6. Convert the put-credit spread strategy from scanner-only behavior to normal strategy behavior:
   consume node data, select a candidate, submit a Nautilus order list, and let the adapter own
   broker I/O.
7. Move close and reconciliation behavior behind the same execution adapter surface.

## Done Criteria For The First Slice

- A Python `TradingNode` can cache selected Alpaca option contracts for one underlying.
- The node can request or poll option snapshots for those contracts.
- A selected put-credit candidate can be converted into a Nautilus `SubmitOrderList`.
- The Alpaca execution client can submit and cancel that order list in paper.
- The node emits normal execution events and account/position reports.
- No strategy code posts directly to Alpaca REST.
- Existing Rust runtime controls and ledgers remain available as operator projections during the
  transition.
