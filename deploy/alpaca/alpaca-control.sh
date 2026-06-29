#!/usr/bin/env bash
set -euo pipefail

CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
STATE_HOME="${XDG_STATE_HOME:-$HOME/.local/state}"
ALPACA_CONFIG_HOME="${NAUTILUS_ALPACA_CONFIG_HOME:-$CONFIG_HOME/nautilus-trader/alpaca}"
ALPACA_STATE_HOME="${NAUTILUS_ALPACA_STATE_HOME:-$STATE_HOME/nautilus_trader/alpaca}"
DEFAULT_ACCOUNT="${NAUTILUS_ALPACA_DEFAULT_ACCOUNT:-paper-main}"
REPO="${NAUTILUS_ALPACA_REPO:-$HOME/Projects/nautilus_trader}"
DEFAULT_ENV_FILE="$REPO/.env"
ACCOUNT_ENV_DIR="${NAUTILUS_ALPACA_ACCOUNT_ENV_DIR:-$ALPACA_CONFIG_HOME/accounts}"
ACCOUNT_CONFIG_DIR="${NAUTILUS_ALPACA_ACCOUNT_CONFIG_DIR:-$ALPACA_CONFIG_HOME/configs}"
NAUTILUS_BIN="${NAUTILUS_BIN:-$HOME/.local/bin/nautilus}"
ENGINE_BIN="${NAUTILUS_ALPACA_RUNNER_BIN:-$HOME/.local/bin/alpaca-options-node}"
ALERTS_ENV_FILE="${NAUTILUS_ALPACA_ALERTS_ENV_FILE:-$ALPACA_CONFIG_HOME/alerts.env}"
FLEET_CONFIG_FILE="${NAUTILUS_ALPACA_FLEET_CONFIG:-$ALPACA_CONFIG_HOME/fleet.toml}"

usage() {
  cat <<EOF
usage: $(basename "$0") [--account ACCOUNT] <command> [args]

Account aliases:
  main, default, paper-main       primary defined-risk paper account
  paper-defined-risk             shared SPY/QQQ defined-risk paper account
  paper-undefined-risk           undefined-risk paper account

Commands:
  accounts                       list known local account ids
  status                         systemd status plus operator summary for one account
  operator [ARGS...]             run Nautilus Alpaca status for one account
  fleet [ARGS...]                run Nautilus Alpaca fleet status
  health                         lightweight service/account health check
  today                          compact fleet status
  performance [--all] [ARGS...]  summarize candidate history and broker-fill PnL
  alerts candidates [ARGS...]    send or dry-run Discord candidate alerts from Postgres
  alerts performance [ARGS...]   send post-market Discord performance digest
  alerts enable|disable|status   control automatic Discord candidate alerts timer
  alerts performance-enable      enable automatic post-market performance digest timer
  alerts performance-disable     disable automatic post-market performance digest timer
  alerts performance-status      show post-market performance digest timer status
  check-config                   print resolved account config
  validate                       run Alpaca formatting, shell, test, and check commands
  deploy                         build and install local Alpaca runtime files
  rollout                        validate, deploy, restart all services, then summarize health
  start|stop|restart|logs        control one account's user service
  restart-all                    restart all known account services

Examples:
  $(basename "$0") --account paper-undefined-risk check-config
  $(basename "$0") --account paper-defined-risk check-config
  $(basename "$0") today
  $(basename "$0") performance --all
  $(basename "$0") alerts candidates --all --dry-run
  $(basename "$0") alerts candidates --all --send
  $(basename "$0") alerts performance
  $(basename "$0") alerts enable
  $(basename "$0") alerts performance-enable
  $(basename "$0") rollout
EOF
}

normalize_account() {
  case "${1:-$DEFAULT_ACCOUNT}" in
    ""|"main"|"default"|"paper-main")
      printf '%s\n' "paper-main"
      ;;
    *)
      printf '%s\n' "$1"
      ;;
  esac
}

fleet_account_field() {
  local account field
  account="$(normalize_account "$1")"
  field="$2"
  [[ -f "$FLEET_CONFIG_FILE" ]] || return 0
  awk -v account="$account" -v field="$field" '
    /^[[:space:]]*\[\[accounts\]\][[:space:]]*$/ {
      in_account = 1
      matched = 0
      next
    }
    /^[[:space:]]*\[/ && $0 !~ /^[[:space:]]*\[accounts\./ && $0 !~ /^[[:space:]]*\[\[accounts\]\]/ {
      in_account = 0
      matched = 0
      next
    }
    in_account && /^[[:space:]]*id[[:space:]]*=/ {
      value = $0
      sub(/^[^=]*=/, "", value)
      gsub(/^[[:space:]"]+|[[:space:]"]+$/, "", value)
      matched = (value == account)
      next
    }
    in_account && matched && $0 ~ "^[[:space:]]*" field "[[:space:]]*=" {
      value = $0
      sub(/^[^=]*=/, "", value)
      gsub(/^[[:space:]"]+|[[:space:]"]+$/, "", value)
      print value
      exit
    }
  ' "$FLEET_CONFIG_FILE"
}

account_env_file() {
  local account configured
  account="$(normalize_account "$1")"
  configured="$(fleet_account_field "$account" "env_file")"
  if [[ -n "$configured" ]]; then
    printf '%s\n' "$configured"
    return
  fi
  printf '%s\n' "$DEFAULT_ENV_FILE"
}

account_config_file() {
  local account configured
  account="$(normalize_account "$1")"
  configured="$(fleet_account_field "$account" "config_file")"
  if [[ -n "$configured" ]]; then
    printf '%s\n' "$configured"
    return
  fi
  if [[ "$account" == "paper-main" ]]; then
    printf '%s\n' "$ALPACA_CONFIG_HOME/options.toml"
  else
    printf '%s\n' "$ACCOUNT_CONFIG_DIR/$account-options.toml"
  fi
}

account_service() {
  local account configured
  account="$(normalize_account "$1")"
  configured="$(fleet_account_field "$account" "service")"
  if [[ -n "$configured" ]]; then
    printf '%s\n' "$configured"
    return
  fi
  if [[ "$account" == "paper-main" ]]; then
    printf '%s\n' "alpaca-options.service"
  else
    printf '%s\n' "alpaca-options@$account.service"
  fi
}

active_account_service() {
  local account service
  account="$(normalize_account "$1")"
  service="$(account_service "$account")"
  if systemctl --user is-active --quiet "$service"; then
    printf '%s\n' "$service"
  else
    printf '%s\n' "$service"
  fi
}

account_log_dir() {
  local account configured
  account="$(normalize_account "$1")"
  configured="$(fleet_account_field "$account" "log_dir")"
  if [[ -n "$configured" ]]; then
    printf '%s\n' "$configured"
    return
  fi
  if [[ "$account" == "paper-main" ]]; then
    printf '%s\n' "$STATE_HOME/nautilus_trader/logs"
  else
    printf '%s\n' "$ALPACA_STATE_HOME/$account/logs"
  fi
}

account_lock_dir() {
  local account configured
  account="$(normalize_account "$1")"
  configured="$(fleet_account_field "$account" "lock_dir")"
  if [[ -n "$configured" ]]; then
    printf '%s\n' "$configured"
    return
  fi
  if [[ "$account" == "paper-main" ]]; then
    printf '%s\n' "$STATE_HOME/nautilus_trader/locks"
  else
    printf '%s\n' "$ALPACA_STATE_HOME/$account/locks"
  fi
}

known_accounts() {
  if [[ -f "$FLEET_CONFIG_FILE" ]]; then
    awk -F= '
      /^[[:space:]]*id[[:space:]]*=/ {
        value = $2
        gsub(/[[:space:]"]/, "", value)
        if (value != "") {
          print value
        }
      }
    ' "$FLEET_CONFIG_FILE"
    return
  fi

  printf '%s\n' "paper-main"
  if [[ -d "$ACCOUNT_ENV_DIR" ]]; then
    find "$ACCOUNT_ENV_DIR" -maxdepth 1 -type f -name '*.env' -printf '%f\n' \
      | sed 's/\.env$//' \
      | sort
  fi
}

require_executable() {
  local path description
  path="$1"
  description="$2"
  if [[ ! -x "$path" ]]; then
    echo "missing executable $description: $path" >&2
    exit 127
  fi
}

require_command() {
  local name
  name="$1"
  if ! command -v "$name" >/dev/null 2>&1; then
    echo "missing required command: $name" >&2
    exit 127
  fi
}

run_logged() {
  echo "+ $*"
  "$@"
}

require_env_file() {
  local env_file
  env_file="$(account_env_file "$1")"
  if [[ ! -f "$env_file" ]]; then
    echo "missing env file: $env_file" >&2
    exit 2
  fi
}

setup_account_env() {
  local account
  account="$(normalize_account "$1")"
  require_env_file "$account"
  export NAUTILUS_ALPACA_ACCOUNT="$account"
  export NAUTILUS_ALPACA_SERVICE
  export NAUTILUS_ALPACA_ENV_FILE
  export NAUTILUS_ALPACA_REPO="$REPO"
  export NAUTILUS_ALPACA_RUNNER_BIN="$ENGINE_BIN"
  export NAUTILUS_ALPACA_LOG_DIR
  export NAUTILUS_ALPACA_LOCK_DIR
  export ALPACA_CONFIG_PATH

  NAUTILUS_ALPACA_SERVICE="$(account_service "$account")"
  NAUTILUS_ALPACA_ENV_FILE="$(account_env_file "$account")"
  NAUTILUS_ALPACA_LOG_DIR="$(account_log_dir "$account")"
  NAUTILUS_ALPACA_LOCK_DIR="$(account_lock_dir "$account")"
  ALPACA_CONFIG_PATH="$(account_config_file "$account")"
}

run_nautilus_alpaca() {
  require_executable "$NAUTILUS_BIN" "Nautilus CLI"
  "$NAUTILUS_BIN" adapters alpaca "$@"
}

run_operator_status() {
  local account
  account="$(normalize_account "$1")"
  shift
  setup_account_env "$account"
  run_nautilus_alpaca status "$@"
}

run_fleet() {
  run_nautilus_alpaca fleet "$@"
}

run_check_config() {
  local account
  account="$(normalize_account "$1")"
  setup_account_env "$account"
  require_executable "$ENGINE_BIN" "engine binary"
  "$ENGINE_BIN" --check-config
}

run_today() {
  local fleet_json
  require_command jq
  fleet_json="$(mktemp "${TMPDIR:-/tmp}/nautilus-alpaca-fleet.XXXXXX")"
  run_fleet --json > "$fleet_json"
  jq -r '
    "fleet checked_at=\(.checked_at_utc) configured=\(.summary.configured) enabled=\(.summary.enabled) ok=\(.summary.ok) broken=\(.summary.broken) open_orders=\(.summary.open_orders) positions=\(.summary.positions) unmanaged=\(.summary.unmanaged_positions) active_entries=\(.summary.active_entries)"
  ' "$fleet_json"
  jq -r '
    .accounts[]
    | (.operator_status.last_decision.action // "none") as $action
    | (.operator_status.last_decision.reason // "") as $reason
    | "account=\(.id) status=\(.status) engine=\(.engine_state) open_orders=\(.open_orders) positions=\(.positions) active_entries=\(.active_entries) unmanaged=\(.unmanaged_positions) alerts=\(.alerts) last_decision=\($action)\(if $reason == "" then "" else ":" + $reason end)"
  ' "$fleet_json"
  rm -f "$fleet_json"
}

run_performance() {
  local account all
  local -a report_args
  account="$ACCOUNT"
  all="false"
  report_args=()
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --all)
        all="true"
        shift
        ;;
      --account|-a)
        account="$(normalize_account "${2:-}")"
        shift 2
        ;;
      *)
        report_args+=("$1")
        shift
        ;;
    esac
  done
  if [[ "$all" == "true" ]]; then
    for account in $(known_accounts | sort -u); do
      run_performance_for_account "$account" "${report_args[@]}"
    done
  else
    run_performance_for_account "$account" "${report_args[@]}"
  fi
}

run_performance_for_account() {
  local account arg json_output
  account="$(normalize_account "$1")"
  shift
  json_output="false"
  for arg in "$@"; do
    if [[ "$arg" == "--json" ]]; then
      json_output="true"
    fi
  done
  setup_account_env "$account"
  if [[ "$json_output" != "true" ]]; then
    echo "performance account=$account"
  fi
  run_nautilus_alpaca performance "$@"
}

run_alerts() {
  local subcommand
  subcommand="${1:-}"
  if [[ $# -gt 0 ]]; then
    shift
  fi
  case "$subcommand" in
    candidates|candidate)
      run_candidate_alerts "$@"
      ;;
    performance|digest)
      run_performance_digest "$@"
      ;;
    enable)
      systemctl --user enable --now alpaca-candidate-alerts.timer
      systemctl --user list-timers alpaca-candidate-alerts.timer --no-pager
      ;;
    disable)
      systemctl --user disable --now alpaca-candidate-alerts.timer
      ;;
    status)
      systemctl --user status alpaca-candidate-alerts.timer --no-pager || true
      systemctl --user status alpaca-candidate-alerts.service --no-pager || true
      ;;
    performance-enable|digest-enable)
      systemctl --user enable --now alpaca-performance-digest.timer
      systemctl --user list-timers alpaca-performance-digest.timer --no-pager
      ;;
    performance-disable|digest-disable)
      systemctl --user disable --now alpaca-performance-digest.timer
      ;;
    performance-status|digest-status)
      systemctl --user status alpaca-performance-digest.timer --no-pager || true
      systemctl --user status alpaca-performance-digest.service --no-pager || true
      ;;
    *)
      echo "usage: $(basename "$0") alerts candidates [--all] [--send|--dry-run] [ARGS...] | alerts performance [ARGS...] | alerts enable|disable|status|performance-enable|performance-disable|performance-status" >&2
      exit 2
      ;;
  esac
}

run_performance_digest() {
  export NAUTILUS_ALPACA_ALERTS_ENV_FILE="$ALERTS_ENV_FILE"
  run_performance --all --send-discord "$@"
}

run_candidate_alerts() {
  local account all
  local -a alert_args
  account="$ACCOUNT"
  all="false"
  alert_args=()
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --all)
        all="true"
        shift
        ;;
      --account|-a)
        account="$(normalize_account "${2:-}")"
        shift 2
        ;;
      *)
        alert_args+=("$1")
        shift
        ;;
    esac
  done
  if [[ "$all" == "true" ]]; then
    for account in $(known_accounts | sort -u); do
      run_candidate_alerts_for_account "$account" "${alert_args[@]}"
    done
  else
    run_candidate_alerts_for_account "$account" "${alert_args[@]}"
  fi
}

run_candidate_alerts_for_account() {
  local account
  account="$(normalize_account "$1")"
  shift
  setup_account_env "$account"
  export NAUTILUS_ALPACA_ALERTS_ENV_FILE="$ALERTS_ENV_FILE"
  echo "candidate_alerts account=$account alerts_env=$ALERTS_ENV_FILE"
  run_nautilus_alpaca alerts candidates "$@"
}

run_validate() {
  cd "$REPO"
  run_logged cargo fmt -p nautilus-alpaca
  run_logged git diff --check
  run_logged bash -n deploy/alpaca/alpaca-control.sh
  run_logged bash -n deploy/alpaca/alpaca-options-install.sh
  run_logged bash -n deploy/alpaca/alpaca-options-runner.sh
  run_logged cargo test -p nautilus-alpaca --features live --lib
  run_logged cargo check -p nautilus-alpaca --features live --bins
  run_logged cargo check -p nautilus-cli --features alpaca --bin nautilus
}

run_deploy() {
  cd "$REPO"
  run_logged deploy/alpaca/alpaca-options-install.sh
}

restart_all_services() {
  local account service
  for account in $(known_accounts | sort -u); do
    service="$(account_service "$account")"
    echo "restarting $service"
    systemctl --user restart "$service"
  done
}

check_all_services_active() {
  local account service failed
  failed=0
  for account in $(known_accounts | sort -u); do
    service="$(active_account_service "$account")"
    if systemctl --user is-active --quiet "$service"; then
      echo "service=$service active=true"
    else
      echo "service=$service active=false"
      failed=1
    fi
  done
  return "$failed"
}

run_rollout() {
  run_validate
  run_deploy
  restart_all_services
  sleep 2
  check_all_services_active
  run_today "$@"
}

ACCOUNT="$(normalize_account "$DEFAULT_ACCOUNT")"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --account|-a)
      ACCOUNT="$(normalize_account "${2:-}")"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      break
      ;;
  esac
done

COMMAND="${1:-}"
if [[ $# -gt 0 ]]; then
  shift
fi

case "$COMMAND" in
  accounts)
    known_accounts | sort -u
    ;;
  status)
    setup_account_env "$ACCOUNT"
    NAUTILUS_ALPACA_SERVICE="$(active_account_service "$ACCOUNT")"
    systemctl --user status "$NAUTILUS_ALPACA_SERVICE" --no-pager || true
    run_operator_status "$ACCOUNT" || true
    ;;
  operator)
    run_operator_status "$ACCOUNT" "$@"
    ;;
  fleet)
    run_fleet "$@"
    ;;
  health)
    setup_account_env "$ACCOUNT"
    NAUTILUS_ALPACA_SERVICE="$(active_account_service "$ACCOUNT")"
    if ! systemctl --user is-active --quiet "$NAUTILUS_ALPACA_SERVICE"; then
      echo "health=down service=$NAUTILUS_ALPACA_SERVICE"
      exit 1
    fi
    lock_file="$NAUTILUS_ALPACA_LOCK_DIR/alpaca-options.lock"
    log_file="$NAUTILUS_ALPACA_LOG_DIR/alpaca-options.log"
    if [[ ! -f "$lock_file" ]]; then
      echo "health=degraded reason=missing_lock service=$NAUTILUS_ALPACA_SERVICE lock_file=$lock_file"
      exit 1
    fi
    run_operator_status "$ACCOUNT" --json >/dev/null
    echo "health=up service=$NAUTILUS_ALPACA_SERVICE lock_file=$lock_file log_file=$log_file"
    ;;
  today)
    run_today "$@"
    ;;
  performance)
    run_performance "$@"
    ;;
  alerts)
    run_alerts "$@"
    ;;
  check-config)
    run_check_config "$ACCOUNT"
    ;;
  validate)
    run_validate
    ;;
  deploy)
    run_deploy
    ;;
  rollout)
    run_rollout "$@"
    ;;
  start)
    setup_account_env "$ACCOUNT"
    systemctl --user start "$NAUTILUS_ALPACA_SERVICE"
    ;;
  stop)
    setup_account_env "$ACCOUNT"
    systemctl --user stop "$NAUTILUS_ALPACA_SERVICE" || true
    ;;
  restart)
    setup_account_env "$ACCOUNT"
    systemctl --user restart "$NAUTILUS_ALPACA_SERVICE"
    ;;
  restart-all)
    restart_all_services
    ;;
  logs)
    setup_account_env "$ACCOUNT"
    NAUTILUS_ALPACA_SERVICE="$(active_account_service "$ACCOUNT")"
    journalctl --user -u "$NAUTILUS_ALPACA_SERVICE" -f
    ;;
  *)
    usage
    exit 2
    ;;
esac
