# Alpaca Spread Close And Reconciliation Migration

This plan tracks Alpaca close management and startup reconciliation moving from leg/order-list
identity to Nautilus `OptionSpread` identity. Spread-backed close submission has been cut over to
native `OptionSpread` orders; the remaining work is paper proof and reconciliation hardening.

## Target

The runtime should eventually treat one strategy entry as one Nautilus `OptionSpread` position with:

- one spread instrument ID and generic spread symbol,
- signed leg ratios from `OptionSpreadPlan`,
- one opening spread order,
- one closing spread order when risk is reduced or flattened,
- Alpaca MLeg parent and leg evidence mapped back to that spread identity.

Entries without persisted spread identity do not fall back to leg-derived close orders. They block
close submission and emit operator evidence until a native close path is designed.

## Close Order Construction

The close path should derive the close order from the same spread identity used at entry.

1. Load the active entry and rebuild or read its `OptionSpreadPlan`.
2. Register/cache the `OptionSpread` instrument if missing.
3. Price close from the cached spread `QuoteTick`, not from stale candidate premium.
4. Submit one reduce-only `Limit` order on the spread instrument.
5. Let `AlpacaExecutionClient` expand that order to Alpaca MLeg with:
   - `order_side = Sell` to close a bought/opened spread package,
   - `reduce_only = true`,
   - signed ratios from the spread symbol,
   - positive Alpaca `position_intent` sides derived from ratio and order side,
   - signed net limit price from the spread order.

The current `close_quote_from_ticks` leg-pricing helper remains only for management trigger
evaluation and PnL context. The submitted close limit source is the native spread quote.

## Reconciliation Mapping

Startup reconciliation must map broker state into spread identity without losing leg evidence.

Broker order mapping:

- Prefer Alpaca MLeg parent order ID as the broker parent.
- Store child leg order IDs as evidence, not as the primary strategy identity.
- Match known entries by spread instrument ID when available.
- During startup repair, match by spread instrument ID first and use canonical signed leg set plus
  order-list ID or client-order prefix only for legacy entries.

Broker position mapping:

- Group option positions by canonical signed leg set and expiration.
- Recognize partial spread positions and block unmanaged exposure until an operator repairs or imports
  state.
- Treat unmatched naked/extra legs as unmanaged broker state.

Fill mapping:

- Parent MLeg fill events should update the spread entry first.
- Leg fills remain audit evidence for PnL/performance.
- Partial fills keep the spread entry active and block duplicate opens.
- Terminal parent orders without fill mark the entry canceled.

Cancellation mapping:

- Canceling a spread close order should cancel the Alpaca MLeg parent where available.
- If the parent is missing but legs remain open, emit a reconciliation block rather than silently
  canceling unrelated leg orders.

## Rollout Phases

Phase 1: Dry-run close draft

- Build a spread close order draft for active spread entries.
- Emit `close_spread_order_draft` with spread instrument, ratios, quote source, signed close limit,
  and broker submit path.
- Do not submit the spread close order.

Phase 2: Reconciliation read model

- Add a read-only operator command or status section that shows how current broker orders and
  positions would map to spread identity.
- Keep the existing startup reconciliation behavior authoritative.

Phase 3: Spread close submit cutover

- Status: implemented for vertical, debit-spread, and iron-condor entries that carry persisted
  spread metadata.
- Remaining proof: capture a paper broker status/orders smoke during market hours and cancel any
  accepted smoke close order unless deliberately left open.

Phase 4: Startup reconciliation cutover

- Make spread identity the primary match key for entries that contain spread metadata.
- Keep historical order-list reconciliation only for entries created before the cutover.

Phase 5: Native-only cleanup

- Keep operator status, events, and docs aligned to the native spread close path so no retired leg
  close fallback is advertised as operational.

## Risks

- Incorrect ratio/sign mapping could invert close intent.
- Alpaca parent/leg order status can diverge from Nautilus cache state.
- Partial fills may look like unmanaged legs unless grouped by spread identity.
- Existing state entries without spread fields cannot use the native spread close path.
- Spread quotes can be missing or stale; close submission must block rather than fall back silently
  to leg-derived broker orders.

## Follow-Up Beads

- Build dry-run spread close order drafts for active spread entries.
- Add spread reconciliation preview to operator status or a source-neutral ops command.
- Capture paper submit/cancel proof for a reduce-only spread close when a safe active position is
  available.
