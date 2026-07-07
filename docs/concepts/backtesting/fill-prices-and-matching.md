# Fill prices and matching

## Fill modeling philosophy

NautilusTrader treats historical order book and trade data as **immutable** during backtesting. What
happened in the market is preserved exactly as recorded. Fills never modify the underlying book
state.

This addresses a gap in academic literature: most research focuses on live market dynamics where the
book actually evolves. Historical backtesting with frozen snapshots is a distinct engineering
problem: how do we simulate realistic fills against data that doesn't change in response to our
orders?

**Design choices:**

- **Immutable historical data**: Order book and trade data are never modified.
- **Optional consumption tracking**: When `liquidity_consumption=True`, the engine tracks consumed
  liquidity per price level to prevent duplicate fills. See
  [order book immutability](#order-book-immutability) for configuration.
- **Reproducible results**: A fixed `random_seed` pins the probabilistic fill model's PRNG.
  Same-process reruns are expected to match; cross-process reruns may differ in rare cases due to
  hash-ordering effects outside the fill model.

## Fill price determination

The matching engine determines fill prices based on order type, book type, and market state.

### L2/L3 order book data

With full order book depth, fills are determined by actual book simulation:

| Order type             | Fill price                                                  |
| ---------------------- | ----------------------------------------------------------- |
| `MARKET`               | Walks the book, filling at each price level (taker).        |
| `MARKET_TO_LIMIT`      | Walks the book, filling at each price level (taker).        |
| `LIMIT`                | Order's limit price when matched (maker).                   |
| `STOP_MARKET`          | Walks the book when triggered.                              |
| `STOP_LIMIT`           | Order's limit price when triggered and matched.             |
| `MARKET_IF_TOUCHED`    | Walks the book when triggered.                              |
| `LIMIT_IF_TOUCHED`     | Order's limit price when triggered.                         |
| `TRAILING_STOP_MARKET` | Walks the book when activated and triggered.                |
| `TRAILING_STOP_LIMIT`  | Order's limit price when activated, triggered, and matched. |

With L2/L3 data, market-type orders may partially fill across multiple price levels if insufficient
liquidity exists at the top of book. Limit-type orders act as resting orders after triggering and
may remain unfilled if the market doesn't reach the limit price. `MARKET_TO_LIMIT` fills as a taker
first, then rests any remaining quantity as a limit order at its first fill price.

### L1 order book data (quotes, trades, bars)

With only top-of-book data, the same book simulation is used with a single-level book:

| Order type             | BUY fill price | SELL fill price |
| ---------------------- | -------------- | --------------- |
| `MARKET`               | Best ask       | Best bid        |
| `MARKET_TO_LIMIT`      | Best ask       | Best bid        |
| `LIMIT`                | Limit price    | Limit price     |
| `STOP_MARKET`          | Best ask       | Best bid        |
| `STOP_LIMIT`           | Limit price    | Limit price     |
| `MARKET_IF_TOUCHED`    | Best ask       | Best bid        |
| `LIMIT_IF_TOUCHED`     | Limit price    | Limit price     |
| `TRAILING_STOP_MARKET` | Best ask       | Best bid        |
| `TRAILING_STOP_LIMIT`  | Limit price    | Limit price     |

With L1 data, the simulated book has a single price level. Orders fill against the available size at
that level. If an order has remaining quantity after exhausting top-of-book liquidity, market and
marketable limit-style orders will slip one tick to fill the residual.

For bar data specifically, `STOP_MARKET` and `TRAILING_STOP_MARKET` orders may fill at the trigger
price rather than best ask/bid when the bar moves through the trigger during its high/low
processing. See [Stop order fill behavior with bar data](#stop-order-fill-behavior-with-bar-data)
for details.

:::note
Fill models can alter these fill prices. See [Fill models](fill-models.md) for details on
configuring execution simulation.
:::

### Order type semantics

- **Market execution**: Fill at current market price (bid/ask). This models real exchange behavior
  where these orders execute at the best available price after triggering. Exception: with bar data,
  `STOP_MARKET` and `TRAILING_STOP_MARKET` orders triggered during H/L processing fill at the
  trigger price (see below).
- **Limit execution**: Fill at the order's limit price when matched. Provides price guarantee but
  may not fill if the market doesn't reach the limit.

### Stop order fill behavior with bar data

When backtesting with bar data only (no tick data), the matching engine distinguishes between two
scenarios for `STOP_MARKET` and `TRAILING_STOP_MARKET` orders:

**Gap scenario** (bar opens past trigger): When a bar's open price gaps past the trigger price, the
stop triggers immediately and fills at the market price (the open). This models real exchange
behavior where stop-market orders provide no price guarantee during gaps.

Example - SELL `STOP_MARKET` with trigger at 100:

- Previous bar closes at 105.
- Next bar opens at 90 (overnight gap down).
- Stop triggers at open and fills at 90.

**Move-through scenario** (bar moves through trigger): When a bar opens normally and then its high
or low moves through the trigger price, the stop fills at the trigger price. Since we only have OHLC
data, we assume the market moved smoothly through the trigger and the order would have filled there.

Example - SELL `STOP_MARKET` with trigger at 100:

- Bar opens at 102 (no gap).
- Bar low reaches 98, moving through trigger at 100.
- Stop fills at 100 (the trigger price).

This behavior caps potential slippage during orderly market moves while still modeling gap slippage
accurately. For tick-level precision, use quote or trade tick data instead of bars.

## Price protection

Price protection defines an exchange-calculated price boundary that prevents marketable orders from
executing at excessively aggressive prices. This models exchanges like Binance and CME that
implement protection mechanisms for market and stop-market orders.

**Configuration:**

```python
from nautilus_trader.backtest.config import BacktestVenueConfig

venue_config = BacktestVenueConfig(
    name="BINANCE",
    oms_type="NETTING",
    account_type="MARGIN",
    starting_balances=["100_000 USDT"],
    price_protection_points=100,  # 100 points = 1.00 offset for 2-decimal instruments
)
```

**How it works:**

The matching engine calculates the protection boundary from the current best bid/ask at fill time:

- **BUY orders**: `protection_price = ask + (points × price_increment)`
- **SELL orders**: `protection_price = bid - (points × price_increment)`

The engine filters out fills beyond the protection boundary. For example, with
`price_protection_points=100` on an instrument with `price_increment=0.01`:

- Best ask is 1001.00.
- Protection price = 1001.00 + (100 × 0.01) = 1002.00.
- A BUY market order fills only at prices ≤ 1002.00.
- Liquidity at 1003.00 or higher is filtered, leaving the order partially filled.

**Trigger-time semantics:**

The engine computes protection at fill time, not order submission time:

- **Market orders**: Protection computed immediately when the order processes.
- **Stop-market orders**: Protection computed when the stop triggers, using the bid/ask at that
  moment.

This design allows stop orders to be submitted even when the opposite side of the book is empty,
since the engine computes protection later when the stop triggers.

**Order types affected:**

- `MARKET`
- `STOP_MARKET`

Limit orders are unaffected since they already define a price boundary.

:::note
Set `price_protection_points=0` to disable price protection (default behavior).
:::

## Order book immutability

Historical order book data is immutable during backtesting. When your order fills against book
liquidity, the book state remains unchanged. This preserves historical data integrity.

The matching engine can optionally use **per-level consumption tracking** to prevent duplicate fills
while allowing fills when fresh liquidity arrives. This behavior is controlled by the
`liquidity_consumption` configuration option.

**Configuration:**

```python
from nautilus_trader.backtest.config import BacktestVenueConfig

venue_config = BacktestVenueConfig(
    name="SIM",
    oms_type="NETTING",
    account_type="CASH",
    starting_balances=["100_000 USD"],
    liquidity_consumption=True,  # Enable consumption tracking (default: False)
)
```

- `liquidity_consumption=False` (default): Each iteration fills against the full book liquidity
  independently.
  Simpler behavior, assumes you're a small participant whose orders don't meaningfully impact available liquidity.
- `liquidity_consumption=True`: Tracks consumed liquidity per price level. Prevents the same
  displayed liquidity from generating multiple fills. Resets when fresh data arrives at that level.

**How consumption tracking works (when enabled):**

For each price level, the engine maintains:

- `original_size`: The book's quantity when tracking began.
- `consumed`: How much has been filled against this level.

When processing a fill:

1. Check if the book's current size at this level matches `original_size`
2. If different (fresh data arrived), reset the entry: `original_size = current_size`, `consumed =
   0`
3. Calculate `available = original_size - consumed`
4. After filling, increment `consumed` by the fill quantity

**Example:**

1. Order book shows 100 units at ask 100.00. Engine tracks: `(original=100, consumed=0)`.
2. Your BUY order fills 30 units. Engine updates: `(original=100, consumed=30)`. Available = 70.
3. Another BUY order attempts 50 units. Available = 70, so it fills 50. `(original=100,
   consumed=80)`.
4. A delta updates ask 100.00 to 120 units. Engine resets: `(original=120, consumed=0)`.
5. New orders can now fill against the fresh 120 units.

**Passive limit order fills on L1 data:**

With L1 data (quotes, trades, bars), the book has only a single price level per side. When the
market moves through a passive (MAKER) limit order's price, the engine must decide how to handle
remaining order quantity after exhausting displayed liquidity.

| `liquidity_consumption` | Behavior when market moves through passive limit                                                |
| ----------------------- | ----------------------------------------------------------------------------------------------- |
| `False` (default)       | Fill entire order at limit price. Assumes market movement implies sufficient liquidity existed. |
| `True`                  | Fill only against displayed liquidity. Order remains open for subsequent fills.                 |

**Example scenario** (`liquidity_consumption=True`):

1. Quote shows ask 100.10 with 50 units.
2. You place BUY LIMIT at 100.05 for 1000 units (passive, resting below ask).
3. Next quote shows ask 100.00 with 30 units (market moved through your limit).
4. Order fills 30 units against displayed liquidity. 970 units remain open.
5. Next quote shows ask 99.95 with 200 units.
6. Order fills another 200 units. 770 units remain open.
7. Fills continue as fresh liquidity arrives at crossed price levels.

This behavior provides conservative fill simulation: your order only fills against liquidity
actually observed in the data, rather than inferring liquidity from price movements.

**Trade tick liquidity:**

Trade ticks provide evidence of executable liquidity at the trade price. When a trade occurs at a
price level not reflected in the current book, the engine can use the trade quantity as available
liquidity, subject to the same consumption tracking rules (when enabled).

**Trade consumption seeding:**

When using L2/L3 book data and a trade tick triggers order matching (e.g., triggering a resting stop
order), the trade itself consumed liquidity from the book. Before simulating fills for triggered
orders, the engine pre-seeds the consumption maps with the trade's consumed volume. This prevents
triggered orders from filling against liquidity that the triggering trade already consumed. This
seeding is skipped for L1 books, where the trade tick has already updated the single top-of-book
level directly.

For example, if the book has 10 units at the best ask and a BUY trade of size 8 triggers a stop
market BUY for 5 units, the stop order sees only 2 units remaining at best ask (10 - 8) and must
fill the remaining 3 units at the next price level. Without this seeding, the stop would incorrectly
fill all 5 units at the best ask price.

The engine uses a timestamp guard to avoid double-counting: if the book's most recent update
(`ts_last`) is newer than the trade's event time (`ts_event`), seeding is skipped. This handles
exchanges like Binance where depth deltas arrive before the corresponding trade tick, so the book
already reflects the consumed liquidity, so additional seeding would over-penalize fills.

:::note
Fill models can add more sophisticated execution dynamics, including:

- Variable slippage based on order size.
- More complex queue position modeling.

:::

### Known limitations

**No queue position within a level**: Consumption tracking determines *how much* liquidity remains
at a level, but doesn't model *where* your order sits in the queue relative to other participants.
Use `prob_fill_on_limit` to simulate queue position probabilistically.

**Trade-driven fills are opportunistic**: When trade ticks indicate liquidity at a price not in the
book, the engine uses this as fill evidence. However, this represents liquidity that existed
momentarily and may not reflect sustained availability.

## Precision requirements and invariants

The matching engine enforces strict precision invariants to ensure data integrity throughout the
fill pipeline. All prices and quantities must match the instrument's configured precision
(`price_precision` and `size_precision`). Mismatches raise a `RuntimeError` immediately, preventing
silent corruption of fill quantities.

| Data/operation | Field                          | Required precision           | Validation location         |
| -------------- | ------------------------------ | ---------------------------- | --------------------------- |
| `QuoteTick`    | `bid_price`, `ask_price`       | `instrument.price_precision` | `process_quote_tick`        |
| `QuoteTick`    | `bid_size`, `ask_size`         | `instrument.size_precision`  | `process_quote_tick`        |
| `TradeTick`    | `price`                        | `instrument.price_precision` | `process_trade_tick`        |
| `TradeTick`    | `size`                         | `instrument.size_precision`  | `process_trade_tick`        |
| `Bar`          | `open`, `high`, `low`, `close` | `instrument.price_precision` | `process_bar`               |
| `Bar`          | `volume` (base units)          | `instrument.size_precision`  | `process_bar`               |
| `Order`        | `quantity`                     | `instrument.size_precision`  | `process_order`             |
| `Order`        | `price`                        | `instrument.price_precision` | `process_order`             |
| `Order`        | `trigger_price`                | `instrument.price_precision` | `process_order`             |
| `Order`        | `activation_price`\*           | `instrument.price_precision` | `process_order`             |
| Order update   | `quantity`                     | `instrument.size_precision`  | `update_order`              |
| Order update   | `price`, `trigger_price`       | `instrument.price_precision` | `update_order`              |
| Fill           | `fill_qty`                     | `instrument.size_precision`  | `apply_fills`, `fill_order` |
| Fill           | `fill_px`                      | `instrument.price_precision` | `apply_fills`               |

\*`activation_price` is immutable after order submission.

:::warning
`Bar.volume` must be in **base currency units**. Some data providers report quote-currency volume;
convert to base units before loading (divide by price or use provider-specific fields).
:::

:::tip
If you encounter a precision mismatch error, align your data to the instrument:

```python
# Align price/quantity to instrument precision
price = instrument.make_price(raw_price)
qty = instrument.make_qty(raw_qty)
```

Also verify that:

1. The instrument definition matches your data source's precision.
2. Data was not inadvertently rounded or truncated during loading.
3. Custom data loaders preserve the original precision metadata.

:::
