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

Python examples can also load the installed Rust runtime account env files directly:

```bash
python examples/live/alpaca/gap_down_fragile_rebound_paper.py \
  --check-config \
  --broker-paper \
  --alpaca-profile paper-directional
```

`--alpaca-profile paper-directional` resolves to
`~/.config/nautilus-trader/alpaca/accounts/paper-directional.env`. Use `--alpaca-profile
paper-main` for `~/.config/nautilus-trader/alpaca/options-engine.env`, or pass
`--alpaca-env-file <path>` for an explicit env file.

Keep account-specific broker-paper risk gates and strategy sizing in the same profile env file:

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
cargo run -p nautilus-alpaca --features live --bin alpaca-check-account-orders
```

## Load option contracts

This command loads active put option contracts for the requested underlyings. It does not submit
orders.

```bash
cargo run -p nautilus-alpaca --features live --bin alpaca-load-option-contracts -- SPY QQQ
```

Optional expiration filters:

```bash
export ALPACA_CONTRACTS_MIN_EXPIRATION="2026-06-19"
export ALPACA_CONTRACTS_MAX_EXPIRATION="2026-06-26"
cargo run -p nautilus-alpaca --features live --bin alpaca-load-option-contracts -- SPY
```

## Load option snapshots

This command loads active option contracts and then requests snapshots for a bounded number of
symbols. It does not submit orders.

```bash
export ALPACA_SNAPSHOT_CONTRACT_LIMIT=50
cargo run -p nautilus-alpaca --features live --bin alpaca-load-option-snapshots -- SPY
```

## Dry-run put-credit scanner

This command scans for put-credit candidates and prints the top candidate for each underlying. It
does not submit orders.

```bash
cargo run -p nautilus-alpaca --features live --bin alpaca-dry-run-put-credit -- SPY QQQ IWM
```

Optional scanner overrides:

```bash
export ALPACA_DRY_RUN_MIN_DTE=5
export ALPACA_DRY_RUN_MAX_DTE=10
export ALPACA_DRY_RUN_SHORT_DELTA_MIN=0.18
export ALPACA_DRY_RUN_SHORT_DELTA_MAX=0.28
export ALPACA_DRY_RUN_WIDTHS="2,3,5"
cargo run -p nautilus-alpaca --features live --bin alpaca-dry-run-put-credit -- SPY
```

## Validate a multi-leg order payload

This command builds and validates a put-credit multi-leg order payload locally. It prints JSON and
does not submit the order.

```bash
cargo run -p nautilus-alpaca --features live --bin alpaca-validate-mleg-order -- \
  SPY260619P00450000 SPY260619P00445000 0.40 1
```

## Rust operator submit/cancel diagnostic

Only run this intentionally with paper credentials. This is the Rust runtime/operator diagnostic for
submission lifecycle checks; prefer the Python `TradingNode` command above when validating the
standard adapter path. The utility submits one multi-leg paper order and requests cancellation if
Alpaca accepts it and the order is not already terminal.

Use a small quantity and a conservative limit, then verify the account has no unexpected open orders
or positions:

```bash
export ALPACA_TRADING_BASE_URL="https://paper-api.alpaca.markets"
export ALPACA_EXECUTION_POLL_ATTEMPTS=1
export ALPACA_EXECUTION_POST_CANCEL_POLL_ATTEMPTS=3

cargo run -p nautilus-alpaca --features live --bin alpaca-paper-execution-harness -- \
  SPY260619P00450000 SPY260619P00445000 4.95 1

cargo run -p nautilus-alpaca --features live --bin alpaca-check-account-orders
```

If an accepted smoke order remains open after the harness exits, cancel it in the Alpaca paper
dashboard or API before continuing. Do not leave smoke orders working unless that is the explicit
test objective.

## Check the options-engine config

Keep submission, management, and close handling disabled while checking config:

```bash
export ALPACA_SUBMIT=false
export ALPACA_MANAGE=false
export ALPACA_CLOSE=false
export ALPACA_KILL_SWITCH=true

cargo run -p nautilus-alpaca --features live --bin alpaca-options-engine -- --check-config
```

To use an account-specific env file:

```bash
export NAUTILUS_ALPACA_ENV_FILE="$HOME/.config/nautilus-trader/alpaca/options-engine.env"
cargo run -p nautilus-alpaca --features live --bin alpaca-options-engine -- --check-config
```

Copy the sample config files from `deploy/alpaca/` before running the engine as a service:

```bash
mkdir -p "$HOME/.config/nautilus-trader/alpaca"
cp deploy/alpaca/alpaca-options-engine.env.example \
  "$HOME/.config/nautilus-trader/alpaca/options-engine.env"
cp deploy/alpaca/alpaca-options-engine.base.toml.example \
  "$HOME/.config/nautilus-trader/alpaca/base-options-engine.toml"
cp deploy/alpaca/alpaca-options-engine.toml.example \
  "$HOME/.config/nautilus-trader/alpaca/options-engine.toml"
chmod 600 "$HOME/.config/nautilus-trader/alpaca/options-engine.env"
```

Do not enable `ALPACA_SUBMIT`, `ALPACA_MANAGE`, or `ALPACA_CLOSE` until paper credentials,
endpoints, account status, open orders, positions, and risk caps have been verified.

## Installed operator commands

After installing the deployment helpers, use `alpaca-control` for account-aware operations:

```bash
deploy/alpaca/alpaca-options-install.sh

alpaca-control --account paper-main check-config
alpaca-control --account paper-main status
alpaca-control fleet --json
alpaca-control today
alpaca-control ledger-summary --all
alpaca-control alerts candidates --all --dry-run
alpaca-control performance --all
```

The installed config layers are `options-engine.env`, `base-options-engine.toml`,
`options-engine.toml`, and `fleet.toml`. `NAUTILUS_ALPACA_ENV_FILE` selects an account env file;
`ALPACA_CONFIG_PATH` selects the account TOML; `extends = "base-options-engine.toml"` lets account
TOML inherit shared scanner and management defaults. Operational env overrides such as
`ALPACA_SUBMIT`, `ALPACA_MANAGE`, `ALPACA_CLOSE`, `ALPACA_KILL_SWITCH`, and
`ALPACA_MAX_ITERATIONS` take precedence over TOML for those supported fields.

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
