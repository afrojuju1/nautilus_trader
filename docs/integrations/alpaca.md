# Alpaca

Alpaca Markets is a brokerage API for US equities, ETFs, and listed equity options. The current
NautilusTrader Alpaca work is an experimental Rust options runtime with broker connectivity,
option-chain access, a native Rust data-client hook for option instruments, multi-leg order
submission, reconciliation, operator tooling for paper trading, and a Python `TradingNode` path for
exact option snapshot data and option multi-leg order lists.

:::warning
This page documents the current experimental Alpaca options runtime. The standard Python
`TradingNode` data factory supports stock bars plus exact option snapshot quotes/Greeks, and the
Python execution factory supports simple equity/ETF DAY limit orders plus option multi-leg order
lists. Alpaca should not be presented as a full Python live adapter alongside the stable
integrations until this surface has more production proof.
:::

## Examples

Safe live examples are available in
[`examples/live/alpaca/`](../../examples/live/alpaca/README.md).

The examples start with read-only account, option contract, option snapshot, option-chain scan, and
multi-leg payload validation commands. They do not submit orders by default.

## Overview

The Rust Alpaca runtime currently includes the following implemented components:

- `AlpacaHttpClient`: Authenticated REST access for account, positions, orders, option contracts,
  option snapshots, account activities, cancellation, and submission endpoints.
- `AlpacaOptionContractProvider`: Option contract loading and conversion into Nautilus
  `OptionContract` instruments on venue `ALPACA`.
- `AlpacaDataClient`: Rust live data client for exact option-instrument requests/subscriptions and
  cached instrument replay.
- `AlpacaExecutionClient`: Rust execution client for simple option limit orders and multi-leg
  option limit orders, with trade-update and REST reconciliation paths.
- `alpaca-options-node`: Account-level options runtime for paper trading with
  Nautilus strategy hosting, risk gates, management, close handling, candidate ledgers, and
  performance reporting.
- `alpaca-options-node --check-config`: Config-check command for deployed options runtime
  configuration.
- `alpaca-ops`: Unified operator CLI for read-only account status, fleet status, alerts,
  performance reports, and strategy-state sync.
- Separate tooling remains for option-chain scan comparison and multi-leg payload validation.

The Python package exposes config objects, a stock-bar and exact-option snapshot data client, an
equity plus option multi-leg execution client, and the Alpaca-specific put-credit scanner scaffold.
Regular daily-bar strategy examples live under the source-neutral examples strategy package.

## Alpaca documentation

Alpaca publishes its API documentation at [docs.alpaca.markets](https://docs.alpaca.markets/).
Refer to Alpaca's documentation for account setup, API key management, option trading permissions,
market-data entitlements, rate limits, and live-trading requirements.

## Products

The current documented product scope is intentionally narrow.

| Product Type      | Supported | Notes                                                                 |
|-------------------|-----------|-----------------------------------------------------------------------|
| US equity options | ✓         | Primary runtime target. Supports option contracts, snapshots, exact instrument data-client loading, snapshot quotes/Greeks, and option orders. |
| US equities/ETFs  | Partial   | Static instruments, stock bars, and simple DAY limit broker orders are available for Python `TradingNode`. |
| Crypto            | -         | Not part of this adapter/runtime slice.                               |

## Environments

| Environment | Trading REST URL                      | Trade updates WebSocket URL              | Notes                         |
|-------------|---------------------------------------|------------------------------------------|-------------------------------|
| Paper       | `https://paper-api.alpaca.markets`    | `wss://paper-api.alpaca.markets/stream`  | Default and recommended first. |
| Live        | `https://api.alpaca.markets`          | `wss://api.alpaca.markets/stream`        | Use only after paper proof.    |

Market-data REST requests use `https://data.alpaca.markets` by default. Stock feed values are
`iex`, `sip`, and `delayed_sip`. Option feed values are `indicative` and `opra`; available data
depends on the Alpaca account's entitlements.

## Credentials

The Rust runtime resolves credentials from explicit config values first, then from environment
variables:

| Value      | Primary env var          | Fallback env vars                         |
|------------|--------------------------|-------------------------------------------|
| API key    | `APCA_API_KEY_ID`        | `ALPACA_API_KEY`                          |
| API secret | `APCA_API_SECRET_KEY`    | `ALPACA_SECRET_KEY`, `ALPACA_API_SECRET`  |

For local and single-host options runtime utilities, the repo-local `.env` is the normal env file.
Run commands from the repo root, or set `NAUTILUS_ALPACA_REPO` so installed binaries can find:

```bash
/home/ade/Projects/nautilus_trader/.env
```

`NAUTILUS_ALPACA_ENV_FILE` is an explicit override for deliberate account or diagnostic boundaries;
it is not the default operator path.

:::warning
Do not commit real Alpaca credentials. Start with paper endpoints and keep submission disabled
until account status, open orders, positions, and config gates have been checked.
:::

## Symbology

Alpaca option contracts use the symbol returned by Alpaca's option contract API. Nautilus converts
those symbols into instrument IDs on venue `ALPACA`:

```text
<ALPACA_OPTION_SYMBOL>.ALPACA
```

For example, an Alpaca option symbol such as `SPY260619P00450000` becomes:

```text
SPY260619P00450000.ALPACA
```

The adapter keeps the Alpaca symbol as the raw symbol and uses the contract payload for underlying,
expiration, strike, option kind, currency, and multiplier fields.

Scanner and contract-loading commands use unqualified US equity or ETF root symbols such as `SPY`,
`QQQ`, and `IWM` for the underlying.

## Rust Options Runtime Capabilities

| Capability                           | Supported | Notes                                                  |
|--------------------------------------|-----------|--------------------------------------------------------|
| Exact option instrument data client  | ✓         | Rust `DataClient` request/subscription hook for selected contracts. |
| Account query                         | ✓         | Trading account status and balances.                   |
| Position query                        | ✓         | Used for startup and periodic reconciliation.          |
| Open order query                      | ✓         | Nested multi-leg orders are requested where available. |
| Order lookup                          | ✓         | By venue order ID or client order ID.                  |
| Account activity polling              | ✓         | Used for fill and lifecycle repair paths.              |
| Simple option limit order             | ✓         | Option limit payloads with `day` time in force.        |
| Multi-leg option limit order          | ✓         | Two to four legs, signed net limit price, `day` TIF.   |
| Multi-leg parent/leg reconciliation   | ✓         | Maps parent and nested leg updates into Nautilus reports. |
| Cancel order                          | ✓         | Used by smoke, stale-entry, and operator flows.        |
| Modify/replace order                  | -         | Not documented as supported.                           |
| Bracket/OCO/stop/trailing orders      | -         | Not documented as supported for this runtime slice.    |

## Python TradingNode Capabilities

| Capability                    | Supported | Notes                                                  |
|-------------------------------|-----------|--------------------------------------------------------|
| Static US equity instruments  | ✓         | Configured symbols on venue `ALPACA`.                  |
| Stock-bar polling             | ✓         | Externally aggregated stock bars.                      |
| Exact option instruments       | ✓         | Static OCC option contracts on venue `ALPACA`.         |
| Option snapshot quotes/Greeks | ✓         | REST snapshot polling, not native streaming.           |
| Account query                 | ✓         | Trading account status and balances.                   |
| Position query                | ✓         | Used for startup and periodic reconciliation.          |
| Order lookup/status reports   | ✓         | By venue order ID or client order ID.                  |
| Simple equity/ETF limit order | ✓         | Whole-share `DAY` limit buy/sell orders.               |
| Cancel order                  | ✓         | Single order, batch cancel, and cancel-all requests.   |
| Shared equity risk gates      | ✓         | Kill switch, notional caps, buying power, duplicate symbol, and short-sale gates. |
| Shared repo env loading       | ✓         | Python examples can load the same repo-local `.env` used by Rust operator tooling. |
| Trade update stream           | -         | REST reconciliation only in the Python client.         |
| Option multi-leg execution    | ✓         | Two to four option legs through `SubmitOrderList`.      |
| Bracket/OCO/stop/trailing     | -         | Not implemented for the Python client.                 |

## Options engine

The options engine is an opinionated account-level runtime built on the Alpaca adapter components.
It is not the generic adapter surface.

The engine can host multiple strategy families from config:

- Put credit spreads.
- Call credit spreads.
- Iron condors.
- Debit spreads.
- Naked puts.

Risk controls include per-account caps, per-underlying caps, sector caps, open-order caps,
buying-power checks, quote-age checks, spread-quality checks, earnings-calendar gates, submit gates,
manage gates, close gates, and a kill switch.

The default example deployment config keeps `submit`, `manage`, and `close` disabled and keeps the
kill switch enabled.

:::warning
Options trading is risky. Undefined-risk or margin-intensive strategy families must require
explicit configuration, paper proof, account-level caps, and buying-power gates before any live use.
:::

## Limitations

The following gaps should remain explicit until they are implemented and proven:

- The Python `TradingNode` path is intentionally narrow: static equities, exact OCC option
  symbols, stock bars, option snapshot quotes/Greeks, whole-share equity/ETF `DAY` limit orders,
  and option multi-leg `DAY` limit order lists.
- Current Python strategy ports are source-neutral equity/ETF daily-bar strategies used by Alpaca
  examples: `GapDownFragileRebound` and `UpsideGapContinuation`.
- Python shared risk gates are adapter-level guardrails, not a complete portfolio risk system.
- Python execution uses REST reconciliation; trade update WebSocket handling remains in the Rust
  runtime.
- Python option data uses REST snapshot polling; native streaming option market data is not
  documented as a public supported client yet.
- Historical market data is not documented as a public supported client yet.
- Assignment, exercise, and expiry require more first-class lifecycle proof before being advertised
  as complete.
- Full Alpaca API parity is not the goal of the current runtime slice.

## Safe starting flow

Start with read-only and dry-run commands:

```bash
export APCA_API_KEY_ID="YOUR_PAPER_KEY"
export APCA_API_SECRET_KEY="YOUR_PAPER_SECRET"
export ALPACA_TRADING_BASE_URL="https://paper-api.alpaca.markets"

cargo run -p nautilus-alpaca --features live --bin alpaca-ops -- account
cargo run -p nautilus-alpaca --features live --bin alpaca-load-option-contracts -- SPY QQQ
cargo run -p nautilus-alpaca --features live --bin alpaca-load-option-snapshots -- SPY
cargo run -p nautilus-alpaca --features live --bin alpaca-compare-option-chain-scan -- --pretty SPY YYYY-MM-DD
cargo run -p nautilus-alpaca --features live --bin alpaca-validate-mleg-order -- \
  SPY260619P00450000 SPY260619P00445000 0.40 1
```

Before running the live node, check config with submission disabled:

```bash
export ALPACA_SUBMIT=false
export ALPACA_MANAGE=false
export ALPACA_CLOSE=false
export ALPACA_KILL_SWITCH=true

cargo run -p nautilus-alpaca --features live --bin alpaca-options-node -- --check-config
```

Paper order smoke tests must be explicit. Use only paper endpoints and keep size small. The standard
Python adapter smoke path is the options `TradingNode`; use `--cancel-after-submit` so accepted
paper orders are canceled before the node stops:

```bash
python examples/live/alpaca/options_mleg_trading_node.py \
  --broker-paper \
  --confirm-submit \
  --cancel-after-submit \
  --alpaca-profile paper-main \
  --short-symbol SPY270115P00450000 \
  --long-symbol SPY270115P00445000 \
  --short-leg-limit 0.50 \
  --long-leg-limit 0.10 \
  --qty 1 \
  --run-seconds 30
```

Use the Rust harness only as the operator/runtime diagnostic, then verify open orders afterward:

```bash
cargo run -p nautilus-alpaca --features live --bin alpaca-paper-execution-harness -- \
  SPY260619P00450000 SPY260619P00445000 4.95 1
cargo run -p nautilus-alpaca --features live --bin alpaca-ops -- account
```

Both submit paths request cancellation after an accepted non-terminal order. If any smoke order
remains open, cancel it in the Alpaca paper dashboard or API before continuing.

Only enable paper submission deliberately, after verifying credentials, endpoints, account status,
open orders, positions, and risk caps.
