#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
if [[ -x "$SCRIPT_DIR/alpaca-control" ]]; then
  exec "$SCRIPT_DIR/alpaca-control" "$@"
fi
exec "$SCRIPT_DIR/alpaca-control.sh" "$@"
