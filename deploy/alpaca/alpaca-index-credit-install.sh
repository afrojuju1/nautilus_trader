#!/usr/bin/env bash
set -euo pipefail

REPO="${NAUTILUS_ALPACA_REPO:-$HOME/Projects/nautilus_trader}"
ENV_FILE="${NAUTILUS_ALPACA_ENV_FILE:-$HOME/.config/nautilus-trader/alpaca/index-credit.env}"
CONFIG_FILE="${ALPACA_CONFIG_PATH:-$HOME/.config/nautilus-trader/alpaca/index-credit.toml}"

cd "$REPO"

cargo build --release -p nautilus-alpaca --features live \
  --bin alpaca-index-credit-engine \
  --bin alpaca-operator-status

install -Dm755 target/release/alpaca-index-credit-engine \
  "$HOME/.local/bin/alpaca-index-credit-engine"
install -Dm755 target/release/alpaca-operator-status \
  "$HOME/.local/bin/alpaca-operator-status"
install -Dm755 deploy/alpaca/alpaca-index-credit-runner.sh \
  "$HOME/.local/bin/alpaca-index-credit-runner"
install -Dm755 deploy/alpaca/alpaca-index-credit-control.sh \
  "$HOME/.local/bin/alpaca-index-credit-control"
install -Dm644 deploy/alpaca/alpaca-index-credit.service \
  "$HOME/.config/systemd/user/alpaca-index-credit.service"

if [[ ! -f "$ENV_FILE" ]]; then
  install -Dm600 deploy/alpaca/alpaca-index-credit.env.example "$ENV_FILE"
fi
if [[ ! -f "$CONFIG_FILE" ]]; then
  install -Dm600 deploy/alpaca/alpaca-index-credit.toml.example "$CONFIG_FILE"
fi

systemctl --user daemon-reload

echo "installed alpaca-index-credit runtime"
echo "env_file=$ENV_FILE"
echo "config_file=$CONFIG_FILE"
echo "runner=$HOME/.local/bin/alpaca-index-credit-engine"
echo "operator=$HOME/.local/bin/alpaca-operator-status"
