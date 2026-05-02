#!/usr/bin/env bash
set -euo pipefail

SERVICE="${NAUTILUS_ALPACA_SERVICE:-alpaca-index-credit.service}"
ENV_FILE="${NAUTILUS_ALPACA_ENV_FILE:-$HOME/.config/nautilus-trader/alpaca/index-credit.env}"

usage() {
  cat <<EOF
usage: $(basename "$0") <status|health|start|stop|restart|logs>

Controls the user systemd service: $SERVICE
EOF
}

require_env_file() {
  if [[ ! -f "$ENV_FILE" ]]; then
    echo "missing env file: $ENV_FILE" >&2
    exit 2
  fi
}

case "${1:-}" in
  status)
    systemctl --user status "$SERVICE" --no-pager
    ;;
  health)
    require_env_file
    if ! systemctl --user is-active --quiet "$SERVICE"; then
      echo "health=down service=$SERVICE"
      exit 1
    fi
    # shellcheck disable=SC1090
    set -a; . "$ENV_FILE"; set +a
    lock_file="${NAUTILUS_ALPACA_LOCK_DIR:-$HOME/.local/state/nautilus_trader/locks}/alpaca-index-credit.lock"
    log_file="${NAUTILUS_ALPACA_LOG_DIR:-$HOME/.local/state/nautilus_trader/logs}/alpaca-index-credit.log"
    if [[ ! -f "$lock_file" ]]; then
      echo "health=degraded reason=missing_lock service=$SERVICE lock_file=$lock_file"
      exit 1
    fi
    echo "health=up service=$SERVICE lock_file=$lock_file log_file=$log_file"
    ;;
  start)
    systemctl --user start "$SERVICE"
    ;;
  stop)
    systemctl --user stop "$SERVICE"
    ;;
  restart)
    systemctl --user restart "$SERVICE"
    ;;
  logs)
    journalctl --user -u "$SERVICE" -f
    ;;
  *)
    usage
    exit 2
    ;;
esac
