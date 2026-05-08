# Alpaca Live Examples

These examples exercise the current Rust Alpaca options runtime safely. They are designed for
paper credentials and do not submit orders by default.

The standard Python `TradingNode` Alpaca factories are not wired yet, so this directory uses the
Rust binaries from `nautilus-alpaca`.

## Credentials

Use Alpaca paper credentials first:

```bash
export APCA_API_KEY_ID="YOUR_PAPER_KEY"
export APCA_API_SECRET_KEY="YOUR_PAPER_SECRET"
export ALPACA_TRADING_BASE_URL="https://paper-api.alpaca.markets"
```

The runtime also accepts `ALPACA_API_KEY` for the key and `ALPACA_SECRET_KEY` or
`ALPACA_API_SECRET` for the secret.

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

## Paper submit/cancel smoke test

Only run this intentionally with paper credentials. The smoke utility submits one multi-leg paper
order and requests cancellation if Alpaca accepts it and the order is not already terminal.

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

Candidate ledgers live under
`~/.local/state/nautilus_trader/alpaca/<account-id>/candidate-ledger/`. Performance ledgers and
candidate-outcome ledgers live beside them under `performance-ledger/` and `candidate-outcomes/`.
Performance reports read broker fills and local state for accounting; they do not submit or cancel
orders.
