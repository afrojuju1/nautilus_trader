# Bar-based execution

Bar data provides a summary of market activity with four key prices for each time period (assuming
bars are aggregated by trades):

- **Open**: opening price (first trade)
- **High**: highest price traded
- **Low**: lowest price traded
- **Close**: closing price (last trade)

While this gives us an overview of price movement, we lose some important information that we'd have
with more granular data:

- We don't know in what order the market hit the high and low prices.
- We can't see exactly when prices changed within the time period.
- We don't know the actual sequence of trades that occurred.

This is why Nautilus processes bar data through a system that attempts to maintain the most
realistic yet conservative market behavior possible, despite these limitations. At its core, the
platform always maintains an order book simulation - even when you provide less granular data such
as quotes, trades, or bars (although the simulation will only have a top level book).

:::warning
When using bars for execution simulation (enabled by default with `bar_execution=True` in venue
configurations), Nautilus strictly expects the initialization timestamp (`ts_init`) of each bar to
represent its **closing time**. This ensures accurate chronological processing, prevents look-ahead
bias, and aligns market updates (Open -> High -> Low -> Close) with the moment the bar is complete.

The event timestamp (`ts_event`) can represent either the open or close time of the bar:

- If `ts_event` is at the **close**, ensure `ts_init_delta=0` when processing bars (default).
- If `ts_event` is at the **open**, set `ts_init_delta` equal to the bar's duration to shift
  `ts_init` to the close.

:::

## Bar timestamp convention

If your data source provides bars timestamped at the **opening time** (common in some providers),
you need to ensure `ts_init` is set to the closing time for correct execution simulation. There are
two approaches:

**Approach 1: Adjust data timestamps (recommended)**

- Use adapter-specific configurations like `bars_timestamp_on_close=True` (e.g., for Bybit or
  Databento adapters) to handle this automatically during data ingestion.
- For custom data, manually shift the timestamps by the bar duration before loading (e.g., add 1
  minute for `1-MINUTE` bars).
- This approach is clearest because the data itself reflects the close time.

**Approach 2: Use `ts_init_delta` parameter**

- When calling `BarDataWrangler.process()`, set `ts_init_delta` to the bar's duration in nanoseconds
  (e.g., `60_000_000_000` for 1-minute bars).
- The wrangler computes `ts_init = ts_event + ts_init_delta`, shifting execution timing to the
  close.
- Use this when you cannot or prefer not to modify source data timestamps.

Always verify your data's timestamp convention with a small sample to avoid simulation inaccuracies.
Incorrect timestamp handling can lead to look-ahead bias and unrealistic backtest results.

## Processing bar data

Even when you provide bar data, Nautilus maintains an internal order book for each instrument, as a
real venue would.

1. **Time processing**:
   - Nautilus uses `ts_init` for execution timing, and `ts_init` must represent the
     bar close. This represents the moment when the bar is fully formed and its
     aggregation is complete.
   - The event timestamp (`ts_event`) represents when the data event occurred and
     may differ from `ts_init` depending on your data source:
     - If your bars are timestamped at the **close** (the recommended default),
       use `ts_init_delta=0` in `BarDataWrangler` so that `ts_init = ts_event`.
     - If your bars are timestamped at the **open**, set `ts_init_delta` to the
       bar's duration in nanoseconds (e.g., 60_000_000_000 for 1-minute bars) to
       shift `ts_init` to the close time.
   - The platform sequences events by `ts_init`, preventing look-ahead bias in your
     backtests.

:::note[Exceptions for bar execution]
Bars will **not** be processed for execution (and will not update the order book) in the following
cases:

- **Internally aggregated bars**: Bars with `AggregationSource.INTERNAL` are skipped to avoid
  processing bars that are derived from already-processed tick data.
- **Non-L1 book types**: When the venue's `book_type` is configured as `L2_MBP` or `L3_MBO`, bar
  data is ignored for execution processing, as bars are derived from top-of-book prices only.

In these cases, bars will still be received by strategies for analytics and decision-making, but
they won't trigger order matching or update the simulated order book.
:::

2. **Price processing**:
   - The platform converts each bar's OHLC prices into a sequence of market updates.
   - By default, updates follow the order: Open -> High -> Low -> Close
     (configurable via `bar_adaptive_high_low_ordering`).
   - If you provide multiple timeframes (like both 1-minute and 5-minute bars),
     the platform uses the more granular data for highest accuracy.

3. **Executions**:
   - When you place orders, they interact with the simulated order book as they
     would on a real venue.
   - For MARKET orders, execution happens at the current simulated market price
     plus any configured latency.
   - For LIMIT orders working in the market, they execute if any of the bar's
     prices reach or cross your limit price.
   - The matching engine continuously processes orders as OHLC prices move, rather
     than waiting for complete bars.

## OHLC price simulation

During backtest execution, each bar is converted into a sequence of four price points:

1. Opening price
2. High price *(Order between High/Low is configurable. See `bar_adaptive_high_low_ordering`
   below.)*
3. Low price
4. Closing price

The trading volume for that bar is **split evenly** among these four points (25% each), with any
remainder added to the closing price trade to preserve total volume. In marginal cases, if the bar's
volume divided by 4 is less than the instrument's minimum `size_increment`, we use the minimum
`size_increment` per price point to ensure valid market activity (e.g., 1 contract for CME group
exchanges).

How these price points are sequenced can be controlled via the `bar_adaptive_high_low_ordering`
parameter when configuring a venue.

Nautilus supports two modes of bar processing:

1. **Fixed ordering** (`bar_adaptive_high_low_ordering=False`, default)
   - Processes every bar in a fixed sequence: `Open -> High -> Low -> Close`.
   - Simple and deterministic approach.

2. **Adaptive ordering** (`bar_adaptive_high_low_ordering=True`)
   - Uses bar structure to estimate likely price path:
     - If Open is closer to High: processes as `Open -> High -> Low -> Close`.
     - If Open is closer to Low: processes as `Open -> Low -> High -> Close`.
   - [Research](https://gist.github.com/stefansimik/d387e1d9ff784a8973feca0cde51e363)
     shows this approach achieves ~75-85% accuracy in predicting correct High/Low
     sequence (compared to statistical ~50% accuracy with fixed ordering).
   - This is particularly important when both take-profit and stop-loss levels occur
     within the same bar: the sequence determines which order fills first.

Here's how to configure adaptive bar ordering for a venue, including account setup:

```python
from nautilus_trader.backtest.engine import BacktestEngine
from nautilus_trader.model.enums import OmsType, AccountType
from nautilus_trader.model import Money, Currency

# Initialize the backtest engine
engine = BacktestEngine()

# Add a venue with adaptive bar ordering and required account settings
engine.add_venue(
    venue=venue,  # Your Venue identifier, e.g., Venue("BINANCE")
    oms_type=OmsType.NETTING,
    account_type=AccountType.CASH,
    starting_balances=[Money(10_000, Currency.from_str("USDT"))],
    bar_adaptive_high_low_ordering=True,  # Enable adaptive ordering of High/Low bar prices
)
```

## Order submission timing

Bar N's OHLC sequence processes before `on_bar(N)` fires. Without a `LatencyModel`, an order
submitted from `on_bar` settles immediately and matches against the current book, whose top reflects
bar N's close.

Attach a `LatencyModel` to the venue to defer the order's effective arrival. With bar-only data and
no intervening timer events, the order settles after the next bar's OHLC sweep, so the fill price is
that bar's close (or a later bar's close if latency exceeds the bar interval). Finer-grained data
(quotes, trades) or timer-driven settlement between bars can drain the order earlier, against the
book as it stands at that point:

```python
from nautilus_trader.backtest.models import LatencyModel

engine.add_venue(
    venue=venue,
    oms_type=OmsType.NETTING,
    account_type=AccountType.CASH,
    starting_balances=[Money(10_000, Currency.from_str("USDT"))],
    latency_model=LatencyModel(base_latency_nanos=1_000_000_000),  # 1 second
)
```

:::note
A native "next-bar-open" execution mode is not provided. A bar's `ts_init` is its close timestamp,
so the open price is only known once the bar arrives. Filling at that open from a signal generated
on the prior bar would require look-ahead.
:::

## Internal bar aggregation timing

When aggregating time bars internally from tick data, the data engine uses timers to close bars at
interval boundaries. A timing edge case occurs when data arrives at the exact bar close timestamp:
the timer may fire before processing boundary data.

Configure `time_bars_build_delay` in `DataEngineConfig` to delay bar close timers:

```python
from nautilus_trader.config import BacktestEngineConfig
from nautilus_trader.data.config import DataEngineConfig

config = BacktestEngineConfig(
    data_engine=DataEngineConfig(
        time_bars_build_delay=1,  # Microseconds
    ),
)
```

:::tip
A small delay (1 microsecond) ensures boundary data is processed before the bar closes. Useful when
tick data clusters at round interval timestamps.
:::

:::note
Only affects internally aggregated bars (`AggregationSource.INTERNAL`).
:::
