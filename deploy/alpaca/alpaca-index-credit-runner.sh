#!/usr/bin/env bash
set -euo pipefail

REPO="${NAUTILUS_ALPACA_REPO:-$HOME/Projects/nautilus_trader}"
LOG_DIR="${NAUTILUS_ALPACA_LOG_DIR:-$HOME/.local/state/nautilus_trader/logs}"
LOCK_DIR="${NAUTILUS_ALPACA_LOCK_DIR:-$HOME/.local/state/nautilus_trader/locks}"
LOCK_FILE="$LOCK_DIR/alpaca-index-credit.lock"
LOG_FILE="$LOG_DIR/alpaca-index-credit.log"
PATH="$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin:${PATH:-}"

mkdir -p "$LOG_DIR" "$LOCK_DIR"

exec 9>"$LOCK_FILE"
if ! flock -n 9; then
  echo "$(date -Is) another alpaca-index-credit runner already holds $LOCK_FILE" | tee -a "$LOG_FILE" >&2
  exit 75
fi

cd "$REPO"

echo "$(date -Is) starting alpaca-index-credit runner repo=$REPO" >> "$LOG_FILE"
exec cargo run -p nautilus-alpaca --features live --bin alpaca-index-put-credit-entry >> "$LOG_FILE" 2>&1
