#!/usr/bin/env bash
set -euo pipefail

REPO="${NAUTILUS_ALPACA_REPO:-$HOME/Projects/nautilus_trader}"
RUNNER_BIN="${NAUTILUS_ALPACA_RUNNER_BIN:-$HOME/.local/bin/alpaca-options-engine}"
LOG_DIR="${NAUTILUS_ALPACA_LOG_DIR:-$HOME/.local/state/nautilus_trader/logs}"
LOCK_DIR="${NAUTILUS_ALPACA_LOCK_DIR:-$HOME/.local/state/nautilus_trader/locks}"
LOCK_FILE="$LOCK_DIR/alpaca-options.lock"
LOG_FILE="$LOG_DIR/alpaca-options.log"

mkdir -p "$LOG_DIR" "$LOCK_DIR"

if [[ ! -x "$RUNNER_BIN" ]]; then
  echo "$(date -Is) missing executable runner binary: $RUNNER_BIN" | tee -a "$LOG_FILE" >&2
  exit 127
fi

exec 9>"$LOCK_FILE"
if ! flock -n 9; then
  echo "$(date -Is) another alpaca-options runner already holds $LOCK_FILE" | tee -a "$LOG_FILE" >&2
  exit 75
fi

cd "$REPO"

echo "$(date -Is) starting alpaca-options runner repo=$REPO bin=$RUNNER_BIN" >> "$LOG_FILE"
exec "$RUNNER_BIN" >> "$LOG_FILE" 2>&1
