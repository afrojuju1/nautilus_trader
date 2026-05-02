#!/usr/bin/env bash
set -euo pipefail

REPO="${NAUTILUS_ALPACA_REPO:-$HOME/Projects/nautilus_trader}"
ENV_FILE="${NAUTILUS_ALPACA_ENV_FILE:-$HOME/.config/nautilus-trader/alpaca/index-credit.env}"

cd "$REPO"

cargo build --release -p nautilus-alpaca --features live \
  --bin alpaca-index-put-credit-entry \
  --bin alpaca-operator-status

install -Dm755 target/release/alpaca-index-put-credit-entry \
  "$HOME/.local/bin/alpaca-index-put-credit-entry"
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

systemctl --user daemon-reload

echo "installed alpaca-index-credit runtime"
echo "env_file=$ENV_FILE"
echo "runner=$HOME/.local/bin/alpaca-index-put-credit-entry"
echo "operator=$HOME/.local/bin/alpaca-operator-status"
