# Alpaca

Alpaca Markets is a brokerage API for US equities, ETFs, and listed equity options. The current
NautilusTrader Alpaca work is an experimental Rust options runtime with broker connectivity,
option-chain access, multi-leg order submission, reconciliation, and operator tooling for paper
trading.

:::warning
This page documents the current experimental Alpaca options runtime. The standard Python
`TradingNode` data and execution factories are not wired to live clients yet, so Alpaca should not
be presented as a full Python live adapter alongside the stable integrations.
:::

## Examples

Safe live examples are available in
[`examples/live/alpaca/`](https://github.com/nautechsystems/nautilus_trader/tree/develop/examples/live/alpaca/).

The examples start with read-only account, option contract, option snapshot, dry-run scanner, and
multi-leg payload validation commands. They do not submit orders by default.

## Overview

The Rust Alpaca runtime currently includes the following implemented components:

- `AlpacaHttpClient`: Authenticated REST access for account, positions, orders, option contracts,
  option snapshots, account activities, cancellation, and submission endpoints.
- `AlpacaOptionContractProvider`: Option contract loading and conversion into Nautilus
  `OptionContract` instruments on venue `ALPACA`.
- `AlpacaExecutionClient`: Rust execution client for simple option limit orders and multi-leg
  option limit orders, with trade-update and REST reconciliation paths.
- `alpaca-options-engine`: Account-level options runtime for paper trading with strategy hosting,
  risk gates, management, close handling, candidate ledgers, and performance reporting.
- Operator binaries for read-only account status, option-chain inspection, dry-run scanning,
  multi-leg payload validation, fleet status, alerts, and performance reports.

The Python package exposes config objects and placeholder live-client factories. Those factories
raise `NotImplementedError` until the Python `TradingNode` integration path is deliberately wired.

## Alpaca documentation

Alpaca publishes its API documentation at [docs.alpaca.markets](https://docs.alpaca.markets/).
Refer to Alpaca's documentation for account setup, API key management, option trading permissions,
market-data entitlements, rate limits, and live-trading requirements.

## Products

The current documented product scope is intentionally narrow.

| Product Type      | Supported | Notes                                                                 |
|-------------------|-----------|-----------------------------------------------------------------------|
| US equity options | ✓         | Primary runtime target. Supports option contracts, snapshots, and option orders. |
| US equities/ETFs  | -         | Account and position payloads may include equities, but public order support is not documented yet. |
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

For deployed options-engine utilities, `NAUTILUS_ALPACA_ENV_FILE` can point to an account-specific
env file. If unset, the runtime looks for:

```bash
~/.config/nautilus-trader/alpaca/options-engine.env
```

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

For example, an Alpaca option symbol such as `SPY260116P00450000` becomes:

```text
SPY260116P00450000.ALPACA
```

The adapter keeps the Alpaca symbol as the raw symbol and uses the contract payload for underlying,
expiration, strike, option kind, currency, and multiplier fields.

Scanner and contract-loading commands use unqualified US equity or ETF root symbols such as `SPY`,
`QQQ`, and `IWM` for the underlying.

## Execution capabilities

| Capability                           | Supported | Notes                                                  |
|--------------------------------------|-----------|--------------------------------------------------------|
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
| Standard Python `TradingNode` factory | -         | Python factories are placeholders.                     |

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

- The Python `AlpacaLiveDataClientFactory` and `AlpacaLiveExecClientFactory` are placeholders.
- A standard Python `TradingNode` Alpaca setup is not supported yet.
- Streaming market data is not documented as a public supported client yet.
- Historical market data is not documented as a public supported client yet.
- Equity order support is not documented yet.
- Assignment, exercise, and expiry require more first-class lifecycle proof before being advertised
  as complete.
- Full Alpaca API parity is not the goal of the current runtime slice.

## Safe starting flow

Start with read-only and dry-run commands:

```bash
export APCA_API_KEY_ID="YOUR_PAPER_KEY"
export APCA_API_SECRET_KEY="YOUR_PAPER_SECRET"
export ALPACA_TRADING_BASE_URL="https://paper-api.alpaca.markets"

cargo run -p nautilus-alpaca --features live --bin alpaca-check-account-orders
cargo run -p nautilus-alpaca --features live --bin alpaca-load-option-contracts -- SPY QQQ
cargo run -p nautilus-alpaca --features live --bin alpaca-load-option-snapshots -- SPY
cargo run -p nautilus-alpaca --features live --bin alpaca-dry-run-put-credit -- SPY QQQ
cargo run -p nautilus-alpaca --features live --bin alpaca-validate-mleg-order -- \
  SPY260116P00450000 SPY260116P00445000 0.40 1
```

Before running the account engine, check config with submission disabled:

```bash
export ALPACA_SUBMIT=false
export ALPACA_MANAGE=false
export ALPACA_CLOSE=false
export ALPACA_KILL_SWITCH=true

cargo run -p nautilus-alpaca --features live --bin alpaca-options-engine -- --check-config
```

Only enable paper submission deliberately, after verifying credentials, endpoints, account status,
open orders, positions, and risk caps.
