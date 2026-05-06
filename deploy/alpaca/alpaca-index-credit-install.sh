#!/usr/bin/env bash
set -euo pipefail

REPO="${NAUTILUS_ALPACA_REPO:-$HOME/Projects/nautilus_trader}"
ENV_FILE="${NAUTILUS_ALPACA_ENV_FILE:-$HOME/.config/nautilus-trader/alpaca/index-credit.env}"
BASE_CONFIG_FILE="${ALPACA_BASE_CONFIG_PATH:-$HOME/.config/nautilus-trader/alpaca/base-index-credit.toml}"
CONFIG_FILE="${ALPACA_CONFIG_PATH:-$HOME/.config/nautilus-trader/alpaca/index-credit.toml}"
FLEET_CONFIG_FILE="${NAUTILUS_ALPACA_FLEET_CONFIG:-$HOME/.config/nautilus-trader/alpaca/fleet.toml}"
ACCOUNT_ENV_DIR="${NAUTILUS_ALPACA_ACCOUNT_ENV_DIR:-$HOME/.config/nautilus-trader/alpaca/accounts}"
ACCOUNT_CONFIG_DIR="${NAUTILUS_ALPACA_ACCOUNT_CONFIG_DIR:-$HOME/.config/nautilus-trader/alpaca/configs}"

cd "$REPO"

cargo build --release -p nautilus-alpaca --features live \
  --bin alpaca-index-credit-engine \
  --bin alpaca-operator-status \
  --bin alpaca-fleet-status

install -Dm755 target/release/alpaca-index-credit-engine \
  "$HOME/.local/bin/alpaca-index-credit-engine"
install -Dm755 target/release/alpaca-operator-status \
  "$HOME/.local/bin/alpaca-operator-status"
install -Dm755 target/release/alpaca-fleet-status \
  "$HOME/.local/bin/alpaca-fleet-status"
install -Dm755 deploy/alpaca/alpaca-index-credit-runner.sh \
  "$HOME/.local/bin/alpaca-index-credit-runner"
install -Dm755 deploy/alpaca/alpaca-control.sh \
  "$HOME/.local/bin/alpaca-control"
install -Dm755 deploy/alpaca/alpaca-index-credit-control.sh \
  "$HOME/.local/bin/alpaca-index-credit-control"
install -Dm644 deploy/alpaca/alpaca-index-credit.service \
  "$HOME/.config/systemd/user/alpaca-index-credit.service"
install -Dm644 deploy/alpaca/alpaca-index-credit@.service \
  "$HOME/.config/systemd/user/alpaca-index-credit@.service"

if [[ ! -f "$ENV_FILE" ]]; then
  install -Dm600 deploy/alpaca/alpaca-index-credit.env.example "$ENV_FILE"
fi
if [[ ! -f "$BASE_CONFIG_FILE" ]]; then
  install -Dm600 deploy/alpaca/alpaca-index-credit.base.toml.example "$BASE_CONFIG_FILE"
fi
if [[ ! -f "$CONFIG_FILE" ]]; then
  install -Dm600 deploy/alpaca/alpaca-index-credit.toml.example "$CONFIG_FILE"
fi
if [[ ! -f "$FLEET_CONFIG_FILE" ]]; then
  install -Dm600 deploy/alpaca/alpaca-fleet.toml.example "$FLEET_CONFIG_FILE"
fi
install -d -m 700 "$ACCOUNT_ENV_DIR" "$ACCOUNT_CONFIG_DIR"

systemctl --user daemon-reload

echo "installed alpaca-index-credit runtime"
echo "env_file=$ENV_FILE"
echo "base_config_file=$BASE_CONFIG_FILE"
echo "config_file=$CONFIG_FILE"
echo "fleet_config_file=$FLEET_CONFIG_FILE"
echo "account_env_dir=$ACCOUNT_ENV_DIR"
echo "account_config_dir=$ACCOUNT_CONFIG_DIR"
echo "runner=$HOME/.local/bin/alpaca-index-credit-engine"
echo "operator=$HOME/.local/bin/alpaca-operator-status"
echo "fleet_operator=$HOME/.local/bin/alpaca-fleet-status"
echo "control=$HOME/.local/bin/alpaca-control"
echo "legacy_control=$HOME/.local/bin/alpaca-index-credit-control"
