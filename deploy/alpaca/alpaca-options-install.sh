#!/usr/bin/env bash
set -euo pipefail

REPO="${NAUTILUS_ALPACA_REPO:-$HOME/Projects/nautilus_trader}"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
ALPACA_CONFIG_HOME="${NAUTILUS_ALPACA_CONFIG_HOME:-$CONFIG_HOME/nautilus-trader/alpaca}"
ENV_FILE="$REPO/.env"
BASE_CONFIG_FILE="${ALPACA_BASE_CONFIG_PATH:-$ALPACA_CONFIG_HOME/base-options.toml}"
CONFIG_FILE="${ALPACA_CONFIG_PATH:-$ALPACA_CONFIG_HOME/options.toml}"
FLEET_CONFIG_FILE="${NAUTILUS_ALPACA_FLEET_CONFIG:-$ALPACA_CONFIG_HOME/fleet.toml}"
ACCOUNT_ENV_DIR="${NAUTILUS_ALPACA_ACCOUNT_ENV_DIR:-$ALPACA_CONFIG_HOME/accounts}"
ACCOUNT_CONFIG_DIR="${NAUTILUS_ALPACA_ACCOUNT_CONFIG_DIR:-$ALPACA_CONFIG_HOME/configs}"
ALERTS_ENV_FILE="${NAUTILUS_ALPACA_ALERTS_ENV_FILE:-$ALPACA_CONFIG_HOME/alerts.env}"
PROFILE_MANIFEST_FILE="${NAUTILUS_ALPACA_PROFILE_MANIFEST:-$ALPACA_CONFIG_HOME/paper-profiles.tsv}"

install_runtime_file() {
  local source target
  source="$1"
  target="$2"
  if [[ -f "$target" ]]; then
    return
  fi
  install -Dm600 "$source" "$target"
}

cd "$REPO"

cargo build --release -p nautilus-alpaca --features live,warehouse-clickhouse \
  --bin alpaca-options-node \
  --bin alpaca-compare-option-chain-scan
cargo build --release -p nautilus-cli --features alpaca --bin nautilus

install -Dm755 target/release/alpaca-options-node \
  "$HOME/.local/bin/alpaca-options-node"
install -Dm755 target/release/alpaca-compare-option-chain-scan \
  "$HOME/.local/bin/alpaca-compare-option-chain-scan"
install -Dm755 target/release/nautilus \
  "$HOME/.local/bin/nautilus"
install -Dm755 deploy/alpaca/alpaca-options-runner.sh \
  "$HOME/.local/bin/alpaca-options-runner"
install -Dm755 deploy/alpaca/alpaca-control.sh \
  "$HOME/.local/bin/alpaca-control"
install -Dm644 deploy/alpaca/alpaca-options.service \
  "$HOME/.config/systemd/user/alpaca-options.service"
install -Dm644 deploy/alpaca/alpaca-options@.service \
  "$HOME/.config/systemd/user/alpaca-options@.service"
install -Dm644 deploy/alpaca/alpaca-candidate-alerts.service \
  "$HOME/.config/systemd/user/alpaca-candidate-alerts.service"
install -Dm644 deploy/alpaca/alpaca-candidate-alerts.timer \
  "$HOME/.config/systemd/user/alpaca-candidate-alerts.timer"
install -Dm644 deploy/alpaca/alpaca-performance-digest.service \
  "$HOME/.config/systemd/user/alpaca-performance-digest.service"
install -Dm644 deploy/alpaca/alpaca-performance-digest.timer \
  "$HOME/.config/systemd/user/alpaca-performance-digest.timer"

install_runtime_file deploy/alpaca/alpaca-options.env.example "$ENV_FILE"
install_runtime_file deploy/alpaca/alpaca-options.base.toml.example "$BASE_CONFIG_FILE"
install_runtime_file deploy/alpaca/alpaca-options.toml.example "$CONFIG_FILE"
if [[ ! -f "$FLEET_CONFIG_FILE" ]]; then
  install -Dm600 deploy/alpaca/alpaca-fleet.toml.example "$FLEET_CONFIG_FILE"
fi
install -d -m 700 "$ACCOUNT_ENV_DIR" "$ACCOUNT_CONFIG_DIR"
if [[ ! -f "$ALERTS_ENV_FILE" ]]; then
  install -Dm600 deploy/alpaca/alpaca-alerts.env.example "$ALERTS_ENV_FILE"
fi
install_runtime_file deploy/alpaca/alpaca-paper-profiles.tsv "$PROFILE_MANIFEST_FILE"
install_runtime_file deploy/alpaca/alpaca-options.paper-directional.toml.example "$ACCOUNT_CONFIG_DIR/paper-directional-options.toml"
install_runtime_file deploy/alpaca/alpaca-options.paper-put-credit-spy.toml.example "$ACCOUNT_CONFIG_DIR/paper-put-credit-spy-options.toml"
install_runtime_file deploy/alpaca/alpaca-options.paper-call-credit-qqq.toml.example "$ACCOUNT_CONFIG_DIR/paper-call-credit-qqq-options.toml"
install_runtime_file deploy/alpaca/alpaca-options.paper-undefined-risk.toml.example "$ACCOUNT_CONFIG_DIR/paper-undefined-risk-options.toml"

systemctl --user daemon-reload

echo "installed alpaca-options runtime"
echo "env_file=$ENV_FILE"
echo "base_config_file=$BASE_CONFIG_FILE"
echo "config_file=$CONFIG_FILE"
echo "fleet_config_file=$FLEET_CONFIG_FILE"
echo "alerts_env_file=$ALERTS_ENV_FILE"
echo "profile_manifest_file=$PROFILE_MANIFEST_FILE"
echo "account_env_dir=$ACCOUNT_ENV_DIR"
echo "account_config_dir=$ACCOUNT_CONFIG_DIR"
echo "runner=$HOME/.local/bin/alpaca-options-node"
echo "config_check=$HOME/.local/bin/alpaca-options-node --check-config"
echo "operator=$HOME/.local/bin/nautilus adapters alpaca status"
echo "fleet_operator=$HOME/.local/bin/nautilus adapters alpaca fleet"
echo "candidate_alerts=$HOME/.local/bin/nautilus adapters alpaca alerts candidates"
echo "performance_report=$HOME/.local/bin/nautilus adapters alpaca performance"
echo "control=$HOME/.local/bin/alpaca-control"
echo "candidate_alerts_timer=$HOME/.config/systemd/user/alpaca-candidate-alerts.timer"
echo "performance_digest_timer=$HOME/.config/systemd/user/alpaca-performance-digest.timer"
