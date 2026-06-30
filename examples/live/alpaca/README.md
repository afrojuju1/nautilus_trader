# Alpaca Live Examples

These examples exercise the current Rust Alpaca options runtime and Python `TradingNode` paths
safely. They are designed for paper credentials and do not submit broker orders by default.

The standard Python `TradingNode` Alpaca data factory supports static US equity instruments, exact
OCC option instruments, stock bars, and option snapshot quotes/Greeks. The Python execution factory
supports simple US equity/ETF `DAY` limit orders and option multi-leg order lists. Python node
examples use the Nautilus sandbox execution client unless `--broker-paper` is passed explicitly.

## Credentials

Use Alpaca paper credentials first:

```bash
export APCA_API_KEY_ID="YOUR_PAPER_KEY"
export APCA_API_SECRET_KEY="YOUR_PAPER_SECRET"
export ALPACA_TRADING_BASE_URL="https://paper-api.alpaca.markets"
```

The runtime also accepts `ALPACA_API_KEY` for the key and `ALPACA_SECRET_KEY` or
`ALPACA_API_SECRET` for the secret.

Python examples also load the repo-local `.env` used by the Rust operator tools:

```bash
python examples/live/alpaca/gap_down_fragile_rebound_paper.py \
  --check-config \
  --broker-paper \
  --alpaca-profile paper-directional
```

`--alpaca-profile paper-directional` sets `NAUTILUS_ALPACA_ACCOUNT=paper-directional` and then
loads the shared repo `.env`. Pass `--alpaca-env-file <path>` only for an explicit diagnostic or
account-boundary override.

Keep broker-paper risk gates and strategy sizing in the repo `.env`:

```bash
ALPACA_EQUITY_KILL_SWITCH=false
ALPACA_EQUITY_MAX_ORDER_NOTIONAL=100
ALPACA_EQUITY_MAX_TOTAL_NOTIONAL=250
ALPACA_EQUITY_MAX_BUYING_POWER_PCT=0.05
ALPACA_EQUITY_ALLOW_DUPLICATE_SYMBOL_EXPOSURE=false
ALPACA_EQUITY_ALLOW_SHORT_SELLING=false

ALPACA_GAP_REBOUND_SYMBOLS="SPY,QQQ,IWM,DIA,GLD"
ALPACA_GAP_REBOUND_CAPITAL=100
ALPACA_UPSIDE_GAP_SYMBOLS="FXI,SMH,SOXX"
ALPACA_UPSIDE_GAP_CAPITAL=100
ALPACA_STOCK_FEED=iex
ALPACA_EQUITY_DAILY_RUN_SECONDS=3600
```

## Options multi-leg TradingNode

This is the standard Python adapter smoke path for options. The node registers exact OCC option
instruments, subscribes option snapshot quotes and Greeks through the standard Python data engine,
and can optionally submit one two-leg opening order list to the Alpaca paper broker.

Data-only config check:

```bash
python examples/live/alpaca/options_mleg_trading_node.py --check-config
```

Run read-only snapshot polling:

```bash
python examples/live/alpaca/options_mleg_trading_node.py \
  --alpaca-profile paper-main \
  --run-seconds 120
```

Paper broker submit mode requires an explicit confirmation flag:

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
  --qty 1
```

Use `--cancel-after-submit` for smoke tests so accepted paper orders are canceled through the
standard execution client before the node stops. If you intentionally omit it, verify and clean up
open broker orders afterward.

Useful overrides:

```bash
ALPACA_OPTION_FEED=indicative
ALPACA_OPTION_SNAPSHOT_POLL_SECS=60
ALPACA_OPTIONS_KILL_SWITCH=false
ALPACA_OPTIONS_NODE_RUN_SECONDS=300
```

## GapDownFragileRebound paper node

This command builds a normal Python `TradingNode`, uses Alpaca stock bars for the configured ETFs,
and routes regular Nautilus strategy orders into the Nautilus sandbox execution client.

```bash
python examples/live/alpaca/gap_down_fragile_rebound_paper.py --check-config
python examples/live/alpaca/gap_down_fragile_rebound_paper.py
```

To route regular strategy orders to the Alpaca paper broker account, use:

```bash
python examples/live/alpaca/gap_down_fragile_rebound_paper.py --broker-paper
```

The broker-paper path applies shared account-level equity risk gates in the Alpaca execution
client. Strategy overrides are read from the selected env file:

```bash
ALPACA_GAP_REBOUND_SYMBOLS="SPY,QQQ,IWM,DIA,GLD"
ALPACA_GAP_REBOUND_CAPITAL=10000
ALPACA_STOCK_FEED=iex
```

## UpsideGapContinuation paper node

This is the rule-based ETF strategy ported from `spreads_notebook` package
`upside_gap_continuation_v1`. It uses Alpaca daily bars for `FXI`, `SMH`, and `SOXX`, submits
normal long-only Nautilus orders, and shares the same broker-paper risk gates.

```bash
python examples/live/alpaca/upside_gap_continuation_paper.py --check-config
python examples/live/alpaca/upside_gap_continuation_paper.py
```

Broker-paper mode:

```bash
python examples/live/alpaca/upside_gap_continuation_paper.py \
  --broker-paper \
  --alpaca-profile paper-directional
```

Strategy overrides are read from the selected env file:

```bash
ALPACA_UPSIDE_GAP_SYMBOLS="FXI,SMH,SOXX"
ALPACA_UPSIDE_GAP_CAPITAL=10000
ALPACA_STOCK_FEED=iex
```

## Combined equity daily strategy node

This node registers both migrated daily-bar equity strategies on one Python `TradingNode`, sharing
one Alpaca data client, one Alpaca execution client, and the same account-level equity risk gates.

```bash
python examples/live/alpaca/equity_daily_strategies_paper.py --check-config
python examples/live/alpaca/equity_daily_strategies_paper.py
```

Broker-paper mode:

```bash
python examples/live/alpaca/equity_daily_strategies_paper.py \
  --broker-paper \
  --alpaca-profile paper-directional
```

## Equity paper submit/cancel smoke test

Only run this intentionally with paper credentials. This command submits one whole-share equity
`DAY` limit order, requests cancellation if it is accepted and non-terminal, then checks account,
positions, and open orders.

Use a far-from-market limit and verify the account has no unexpected open orders afterward:

```bash
export ALPACA_TRADING_BASE_URL="https://paper-api.alpaca.markets"

python examples/live/alpaca/alpaca_equity_broker_smoke.py \
  --confirm-submit \
  --alpaca-profile paper-directional \
  --symbol SPY \
  --qty 1 \
  --limit-price 1.00
```

If a smoke order remains open, cancel it in the Alpaca paper dashboard or API before continuing.

## Read-only account status

This command reads account state, positions, and open orders. It does not submit or cancel orders.

```bash
cargo run -p nautilus-cli --features alpaca --bin nautilus -- adapters alpaca account
```

## Load option contracts

This command loads active put option contracts for the requested underlyings. It does not submit
orders.

```bash
cargo run -p nautilus-cli --features alpaca --bin nautilus -- adapters alpaca option-contracts SPY QQQ
```

Optional expiration filters:

```bash
export ALPACA_CONTRACTS_MIN_EXPIRATION="2027-01-15"
export ALPACA_CONTRACTS_MAX_EXPIRATION="2027-01-22"
cargo run -p nautilus-cli --features alpaca --bin nautilus -- adapters alpaca option-contracts SPY
```

## Load option snapshots

This command loads active option contracts and then requests snapshots for a bounded number of
symbols. It does not submit orders.

```bash
export ALPACA_SNAPSHOT_CONTRACT_LIMIT=50
cargo run -p nautilus-cli --features alpaca --bin nautilus -- adapters alpaca option-snapshots SPY
```

## Option-chain scan comparison

This command compares the REST-normalized scanner output with the Nautilus option-chain scanner for
one underlying and expiry. It does not submit orders.

```bash
cargo run -p nautilus-alpaca --features live --bin alpaca-compare-option-chain-scan -- --pretty SPY YYYY-MM-DD
```

## Check the options runtime config

Keep broker order capabilities disabled while checking config:

```bash
export ALPACA_OPEN_ORDERS=false
export ALPACA_CLOSE_ORDERS=false

cargo run -p nautilus-alpaca --features live --bin alpaca-options-node -- --check-config
```

To use an explicit diagnostic or account-boundary env file:

```bash
export NAUTILUS_ALPACA_ENV_FILE="/path/to/account-boundary.env"
cargo run -p nautilus-alpaca --features live --bin alpaca-options-node -- --check-config
```

Copy the sample env and config files before running the runtime as a service:

```bash
cp deploy/alpaca/alpaca-options.env.example .env
chmod 600 .env
mkdir -p "$HOME/.config/nautilus-trader/alpaca"
cp deploy/alpaca/alpaca-options.base.toml.example \
  "$HOME/.config/nautilus-trader/alpaca/base-options.toml"
cp deploy/alpaca/alpaca-options.toml.example \
  "$HOME/.config/nautilus-trader/alpaca/options.toml"
```

Do not enable `ALPACA_OPEN_ORDERS` or `ALPACA_CLOSE_ORDERS` until paper credentials, endpoints,
account status, open orders, positions, and risk caps have been verified.

## Installed operator commands

After installing the deployment helpers, use `alpaca-control` for account-aware operations:

```bash
deploy/alpaca/alpaca-options-install.sh

alpaca-control --account paper-main check-config
alpaca-control --account paper-main status
alpaca-control fleet --json
alpaca-control today
alpaca-control alerts candidates --all --dry-run
alpaca-control performance --all
```

The installed config layers are repo `.env`, `base-options.toml`, `options.toml`, and `fleet.toml`.
`NAUTILUS_ALPACA_ENV_FILE` selects an explicit env-file override; `ALPACA_CONFIG_PATH` selects the
account TOML; `extends = "base-options.toml"` lets account TOML inherit shared scanner and
management defaults. Operational env overrides such as `ALPACA_OPEN_ORDERS`,
`ALPACA_CLOSE_ORDERS`, and `ALPACA_MAX_ITERATIONS` take precedence over TOML for those supported
fields.

Service control is available through either systemd or the wrapper:

```bash
systemctl --user start alpaca-options.service
systemctl --user status alpaca-options.service --no-pager
alpaca-control --account paper-main restart
alpaca-control --account paper-main logs
```

Candidate ledgers, performance ledgers, and candidate outcomes are stored in Postgres through
`ALPACA_STORAGE_DATABASE_URL`. Performance reports read broker fills and persisted state for
accounting; they do not submit or cancel orders. Legacy JSONL ledgers can be imported with
`alpaca-migrate-jsonl-storage`.
