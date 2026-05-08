#!/usr/bin/env bash
set -euo pipefail

CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
STATE_HOME="${XDG_STATE_HOME:-$HOME/.local/state}"
ALPACA_CONFIG_HOME="${NAUTILUS_ALPACA_CONFIG_HOME:-$CONFIG_HOME/nautilus-trader/alpaca}"
ALPACA_STATE_HOME="${NAUTILUS_ALPACA_STATE_HOME:-$STATE_HOME/nautilus_trader/alpaca}"
DEFAULT_ACCOUNT="${NAUTILUS_ALPACA_DEFAULT_ACCOUNT:-paper-main}"
DEFAULT_ENV_FILE="${NAUTILUS_ALPACA_DEFAULT_ENV_FILE:-$ALPACA_CONFIG_HOME/options-engine.env}"
ACCOUNT_ENV_DIR="${NAUTILUS_ALPACA_ACCOUNT_ENV_DIR:-$ALPACA_CONFIG_HOME/accounts}"
ACCOUNT_CONFIG_DIR="${NAUTILUS_ALPACA_ACCOUNT_CONFIG_DIR:-$ALPACA_CONFIG_HOME/configs}"
REPO="${NAUTILUS_ALPACA_REPO:-$HOME/Projects/nautilus_trader}"
ENGINE_BIN="${NAUTILUS_ALPACA_RUNNER_BIN:-$HOME/.local/bin/alpaca-options-engine}"
OPERATOR_BIN="${NAUTILUS_ALPACA_OPERATOR_BIN:-$HOME/.local/bin/alpaca-operator-status}"
FLEET_BIN="${NAUTILUS_ALPACA_FLEET_BIN:-$HOME/.local/bin/alpaca-fleet-status}"
CANDIDATE_ALERTS_BIN="${NAUTILUS_ALPACA_CANDIDATE_ALERTS_BIN:-$HOME/.local/bin/alpaca-candidate-alerts}"
PERFORMANCE_BIN="${NAUTILUS_ALPACA_PERFORMANCE_BIN:-$HOME/.local/bin/alpaca-performance-report}"
ALERTS_ENV_FILE="${NAUTILUS_ALPACA_ALERTS_ENV_FILE:-$ALPACA_CONFIG_HOME/alerts.env}"
OVERRIDE_ENV_FILE=""

cleanup() {
  if [[ -n "$OVERRIDE_ENV_FILE" && -f "$OVERRIDE_ENV_FILE" ]]; then
    rm -f "$OVERRIDE_ENV_FILE"
  fi
}
trap cleanup EXIT

usage() {
  cat <<EOF
usage: $(basename "$0") [--account ACCOUNT] <command> [args]

Account aliases:
  main, default, paper-main       primary defined-risk paper account
  paper-directional              long-premium directional paper account
  paper-undefined-risk           undefined-risk paper account

Commands:
  accounts                       list known local account ids
  status                         systemd status plus operator summary for one account
  operator [ARGS...]             run alpaca-operator-status for one account
  fleet [ARGS...]                run alpaca-fleet-status
  health                         lightweight service/account health check
  today [--date YYYY-MM-DD]      compact fleet and candidate-ledger status
  ledger-summary [--all]         summarize candidate-ledger records and latest decisions
  performance [--all] [ARGS...]  summarize opportunity history and broker-fill PnL
  alerts candidates [ARGS...]    send or dry-run Discord candidate alerts from candidate ledger
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
  scan [PROFILE] [SYMBOLS]       one-shot dry-run candidate scan; disables submit/manage/close
  run-once [SYMBOLS]             one engine iteration using account runtime gates
  ledger                         tail one account's candidate ledger

Scan profiles:
  iron-condor, put-credit, call-credit, credit, directional, naked, naked-1-3dte

Examples:
  $(basename "$0") --account paper-undefined-risk check-config
  $(basename "$0") --account paper-undefined-risk scan naked GDX,SLV
  $(basename "$0") --account paper-directional scan directional XLF,XLK
  $(basename "$0") --account paper-main ledger --lines 20
  $(basename "$0") today
  $(basename "$0") ledger-summary --all
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

account_env_file() {
  local account
  account="$(normalize_account "$1")"
  if [[ "$account" == "paper-main" ]]; then
    printf '%s\n' "$DEFAULT_ENV_FILE"
  else
    printf '%s\n' "$ACCOUNT_ENV_DIR/$account.env"
  fi
}

account_config_file() {
  local account
  account="$(normalize_account "$1")"
  if [[ "$account" == "paper-main" ]]; then
    printf '%s\n' "$ALPACA_CONFIG_HOME/options-engine.toml"
  else
    printf '%s\n' "$ACCOUNT_CONFIG_DIR/$account-options-engine.toml"
  fi
}

account_service() {
  local account
  account="$(normalize_account "$1")"
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
  local account
  account="$(normalize_account "$1")"
  if [[ "$account" == "paper-main" ]]; then
    printf '%s\n' "$STATE_HOME/nautilus_trader/logs"
  else
    printf '%s\n' "$ALPACA_STATE_HOME/$account/logs"
  fi
}

account_lock_dir() {
  local account
  account="$(normalize_account "$1")"
  if [[ "$account" == "paper-main" ]]; then
    printf '%s\n' "$STATE_HOME/nautilus_trader/locks"
  else
    printf '%s\n' "$ALPACA_STATE_HOME/$account/locks"
  fi
}

candidate_ledger_file() {
  local account date
  account="$(normalize_account "$1")"
  date="$2"
  printf '%s\n' "$ALPACA_STATE_HOME/$account/candidate-ledger/$date.jsonl"
}

known_accounts() {
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
  export NAUTILUS_ALPACA_OPERATOR_BIN="$OPERATOR_BIN"
  export NAUTILUS_ALPACA_LOG_DIR
  export NAUTILUS_ALPACA_LOCK_DIR
  export ALPACA_CONFIG_PATH

  NAUTILUS_ALPACA_SERVICE="$(account_service "$account")"
  NAUTILUS_ALPACA_ENV_FILE="$(account_env_file "$account")"
  NAUTILUS_ALPACA_LOG_DIR="$(account_log_dir "$account")"
  NAUTILUS_ALPACA_LOCK_DIR="$(account_lock_dir "$account")"
  ALPACA_CONFIG_PATH="$(account_config_file "$account")"
}

make_account_env_overlay() {
  local account base_env skip_keys
  account="$(normalize_account "$1")"
  shift
  setup_account_env "$account"
  base_env="$NAUTILUS_ALPACA_ENV_FILE"
  OVERRIDE_ENV_FILE="$(mktemp "${TMPDIR:-/tmp}/nautilus-alpaca-env.XXXXXX")"
  chmod 600 "$OVERRIDE_ENV_FILE"
  skip_keys=" ALPACA_SUBMIT ALPACA_MANAGE ALPACA_CLOSE ALPACA_KILL_SWITCH ALPACA_FORCE_FLATTEN ALPACA_CANCEL_AFTER_ACCEPT ALPACA_STRATEGIES ALPACA_DRY_RUN_STRATEGIES ALPACA_MAX_ITERATIONS ALPACA_INTERVAL_SECS ALPACA_IGNORE_ENTRY_WINDOW ALPACA_MAX_ACTIVE_ENTRIES ALPACA_MAX_DAILY_SUBMITS ALPACA_MAX_OPEN_ORDERS ALPACA_MAX_ACTIVE_ENTRIES_PER_UNDERLYING ALPACA_MAX_ACTIVE_ENTRIES_PER_SECTOR NAUTILUS_ALPACA_ACCOUNT NAUTILUS_ALPACA_SERVICE NAUTILUS_ALPACA_REPO NAUTILUS_ALPACA_RUNNER_BIN NAUTILUS_ALPACA_OPERATOR_BIN NAUTILUS_ALPACA_LOG_DIR NAUTILUS_ALPACA_LOCK_DIR ALPACA_CONFIG_PATH "
  awk -v skip_keys="$skip_keys" '
    /^[[:space:]]*($|#)/ { print; next }
    {
      line = $0
      sub(/^[[:space:]]*export[[:space:]]+/, "", line)
      key = line
      sub(/[[:space:]]*=.*/, "", key)
      gsub(/[[:space:]]/, "", key)
      if (index(skip_keys, " " key " ") == 0) {
        print
      }
    }
  ' "$base_env" > "$OVERRIDE_ENV_FILE"
  {
    printf 'NAUTILUS_ALPACA_ACCOUNT=%s\n' "$NAUTILUS_ALPACA_ACCOUNT"
    printf 'NAUTILUS_ALPACA_SERVICE=%s\n' "$NAUTILUS_ALPACA_SERVICE"
    printf 'NAUTILUS_ALPACA_REPO=%s\n' "$NAUTILUS_ALPACA_REPO"
    printf 'NAUTILUS_ALPACA_RUNNER_BIN=%s\n' "$NAUTILUS_ALPACA_RUNNER_BIN"
    printf 'NAUTILUS_ALPACA_OPERATOR_BIN=%s\n' "$NAUTILUS_ALPACA_OPERATOR_BIN"
    printf 'NAUTILUS_ALPACA_LOG_DIR=%s\n' "$NAUTILUS_ALPACA_LOG_DIR"
    printf 'NAUTILUS_ALPACA_LOCK_DIR=%s\n' "$NAUTILUS_ALPACA_LOCK_DIR"
    printf 'ALPACA_CONFIG_PATH=%s\n' "$ALPACA_CONFIG_PATH"
    for assignment in "$@"; do
      printf '%s\n' "$assignment"
    done
  } >> "$OVERRIDE_ENV_FILE"
  export NAUTILUS_ALPACA_ENV_FILE="$OVERRIDE_ENV_FILE"
}

default_profile_for_account() {
  local account
  account="$(normalize_account "$1")"
  case "$account" in
    paper-directional)
      printf '%s\n' "directional"
      ;;
    paper-undefined-risk)
      printf '%s\n' "naked"
      ;;
    *)
      printf '%s\n' "iron-condor"
      ;;
  esac
}

strategies_for_profile() {
  case "$1" in
    "iron-condor"|"iron_condor"|"condor")
      printf '%s\n' "iron_condor"
      ;;
    "put-credit"|"put_credit"|"put")
      printf '%s\n' "put"
      ;;
    "call-credit"|"call_credit"|"call")
      printf '%s\n' "call"
      ;;
    "credit"|"credit-verticals"|"credit_verticals")
      printf '%s\n' "put,call"
      ;;
    "directional"|"debit"|"long-premium"|"long_premium")
      printf '%s\n' "call_debit,put_debit"
      ;;
    "naked"|"undefined-risk"|"undefined_risk"|"short-premium"|"short_premium")
      printf '%s\n' "naked_call,naked_put"
      ;;
    "naked-1-3dte"|"naked_1_3dte"|"1-3dte"|"undefined-risk-1-3dte"|"undefined_risk_1_3dte")
      printf '%s\n' "naked_call_1_3dte,naked_put_1_3dte"
      ;;
    *)
      echo "unsupported scan profile: $1" >&2
      exit 2
      ;;
  esac
}

run_operator_status() {
  local account
  account="$(normalize_account "$1")"
  shift
  setup_account_env "$account"
  require_executable "$OPERATOR_BIN" "operator binary"
  "$OPERATOR_BIN" "$@"
}

run_check_config() {
  local account
  account="$(normalize_account "$1")"
  setup_account_env "$account"
  require_executable "$ENGINE_BIN" "engine binary"
  "$ENGINE_BIN" --check-config
}

run_scan() {
  local account profile symbols strategies ignore_entry_window ignore_risk iterations dry_run
  local -a scan_overrides
  account="$ACCOUNT"
  profile=""
  symbols=""
  ignore_entry_window="true"
  ignore_risk="true"
  iterations="1"
  dry_run="true"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --account|-a)
        account="$(normalize_account "${2:-}")"
        shift 2
        ;;
      --profile|-p)
        profile="${2:-}"
        shift 2
        ;;
      --symbols|-s)
        symbols="${2:-}"
        shift 2
        ;;
      --iterations|-n)
        iterations="${2:-}"
        shift 2
        ;;
      --respect-entry-window)
        ignore_entry_window="false"
        shift
        ;;
      --ignore-entry-window)
        ignore_entry_window="true"
        shift
        ;;
      --respect-risk)
        ignore_risk="false"
        shift
        ;;
      --ignore-risk)
        ignore_risk="true"
        shift
        ;;
      --submit|--live)
        dry_run="false"
        shift
        ;;
      --dry-run)
        dry_run="true"
        shift
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        if [[ -z "$profile" ]]; then
          profile="$1"
        elif [[ -z "$symbols" ]]; then
          symbols="$1"
        else
          echo "unexpected scan argument: $1" >&2
          exit 2
        fi
        shift
        ;;
    esac
  done
  profile="${profile:-$(default_profile_for_account "$account")}"
  strategies="$(strategies_for_profile "$profile")"
  if [[ "$dry_run" == "true" ]]; then
    scan_overrides=(
      "ALPACA_SUBMIT=false"
      "ALPACA_MANAGE=false"
      "ALPACA_CLOSE=false"
      "ALPACA_KILL_SWITCH=false"
      "ALPACA_STRATEGIES=$strategies"
      "ALPACA_DRY_RUN_STRATEGIES=$strategies"
      "ALPACA_MAX_ITERATIONS=$iterations"
      "ALPACA_IGNORE_ENTRY_WINDOW=$ignore_entry_window"
    )
    if [[ "$ignore_risk" == "true" ]]; then
      scan_overrides+=(
        "ALPACA_MAX_ACTIVE_ENTRIES=999"
        "ALPACA_MAX_DAILY_SUBMITS=999"
        "ALPACA_MAX_OPEN_ORDERS=999"
        "ALPACA_MAX_ACTIVE_ENTRIES_PER_UNDERLYING=999"
        "ALPACA_MAX_ACTIVE_ENTRIES_PER_SECTOR=999"
      )
    fi
    make_account_env_overlay "$account" "${scan_overrides[@]}"
  else
    make_account_env_overlay "$account" \
      "ALPACA_STRATEGIES=$strategies" \
      "ALPACA_MAX_ITERATIONS=$iterations" \
      "ALPACA_IGNORE_ENTRY_WINDOW=$ignore_entry_window"
  fi
  require_executable "$ENGINE_BIN" "engine binary"
  echo "scan account=$(normalize_account "$account") profile=$profile strategies=$strategies symbols=${symbols:-config} dry_run=$dry_run ignore_entry_window=$ignore_entry_window ignore_risk=$ignore_risk iterations=$iterations"
  if [[ -n "$symbols" ]]; then
    "$ENGINE_BIN" "$symbols"
  else
    "$ENGINE_BIN"
  fi
}

run_once() {
  local account symbols profile strategies ignore_entry_window iterations
  local -a overrides
  account="$ACCOUNT"
  symbols=""
  profile=""
  strategies=""
  ignore_entry_window=""
  iterations="1"
  overrides=("ALPACA_MAX_ITERATIONS=$iterations")
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --account|-a)
        account="$(normalize_account "${2:-}")"
        shift 2
        ;;
      --profile|-p)
        profile="${2:-}"
        shift 2
        ;;
      --symbols|-s)
        symbols="${2:-}"
        shift 2
        ;;
      --iterations|-n)
        iterations="${2:-}"
        shift 2
        ;;
      --ignore-entry-window)
        ignore_entry_window="true"
        shift
        ;;
      --respect-entry-window)
        ignore_entry_window="false"
        shift
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        if [[ -z "$symbols" ]]; then
          symbols="$1"
        else
          echo "unexpected run-once argument: $1" >&2
          exit 2
        fi
        shift
        ;;
    esac
  done
  overrides=("ALPACA_MAX_ITERATIONS=$iterations")
  if [[ -n "$profile" ]]; then
    strategies="$(strategies_for_profile "$profile")"
    overrides+=("ALPACA_STRATEGIES=$strategies")
  fi
  if [[ -n "$ignore_entry_window" ]]; then
    overrides+=("ALPACA_IGNORE_ENTRY_WINDOW=$ignore_entry_window")
  fi
  make_account_env_overlay "$account" "${overrides[@]}"
  require_executable "$ENGINE_BIN" "engine binary"
  echo "run_once account=$(normalize_account "$account") profile=${profile:-config} symbols=${symbols:-config} iterations=$iterations runtime_gates=account_config"
  if [[ -n "$symbols" ]]; then
    "$ENGINE_BIN" "$symbols"
  else
    "$ENGINE_BIN"
  fi
}

run_ledger() {
  local account date lines mode path
  account="$ACCOUNT"
  date="$(date +%F)"
  lines="25"
  mode="tail"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --account|-a)
        account="$(normalize_account "${2:-}")"
        shift 2
        ;;
      --date)
        date="${2:-}"
        shift 2
        ;;
      --lines|-n)
        lines="${2:-}"
        shift 2
        ;;
      --cat)
        mode="cat"
        shift
        ;;
      --count)
        mode="count"
        shift
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        echo "unexpected ledger argument: $1" >&2
        exit 2
        ;;
    esac
  done
  path="$(candidate_ledger_file "$account" "$date")"
  if [[ ! -f "$path" ]]; then
    echo "missing candidate ledger: $path" >&2
    exit 1
  fi
  case "$mode" in
    cat)
      cat "$path"
      ;;
    count)
      wc -l "$path"
      ;;
    *)
      tail -n "$lines" "$path"
      ;;
  esac
}

ledger_count() {
  local account date type path
  account="$(normalize_account "$1")"
  date="$2"
  type="$3"
  path="$(candidate_ledger_file "$account" "$date")"
  if [[ ! -f "$path" ]]; then
    printf '%s\n' "0"
    return
  fi
  jq -s --arg type "$type" '[.[] | select(.type == $type)] | length' "$path"
}

ledger_reentry_count() {
  local account date path
  account="$(normalize_account "$1")"
  date="$2"
  path="$(candidate_ledger_file "$account" "$date")"
  if [[ ! -f "$path" ]]; then
    printf '%s\n' "0"
    return
  fi
  jq -s '[.[] | select(.type == "scanner_result" and .reason == "daily_duplicate_state")] | length' "$path"
}

run_ledger_summary_one() {
  local account date path records
  account="$(normalize_account "$1")"
  date="$2"
  path="$(candidate_ledger_file "$account" "$date")"
  echo "ledger account=$account date=$date path=$path"
  if [[ ! -f "$path" ]]; then
    echo "  missing=true"
    return
  fi
  records="$(wc -l < "$path" | tr -d ' ')"
  echo "  records=$records"
  echo "  types:"
  jq -r '.type' "$path" | sort | uniq -c | sort -nr | sed 's/^/    /'
  echo "  latest_submit_results:"
  jq -c 'select(.type == "submit_result") | {ts_utc,accepted,rejected,parent_order_id}' "$path" \
    | tail -10 \
    | sed 's/^/    /'
  echo "  latest_decisions:"
  jq -c 'select(.type == "decision") | {ts_utc,action,reason,underlying,strategy,current,limit}' "$path" \
    | tail -10 \
    | sed 's/^/    /'
  echo "  latest_same_day_reentry_blocks:"
  jq -c 'select(.type == "scanner_result" and .reason == "daily_duplicate_state") | {ts_utc,underlying,reason,scope}' "$path" \
    | tail -10 \
    | sed 's/^/    /'
}

run_ledger_summary() {
  local account date all
  account="$ACCOUNT"
  date="$(date +%F)"
  all="false"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --account|-a)
        account="$(normalize_account "${2:-}")"
        shift 2
        ;;
      --date)
        date="${2:-}"
        shift 2
        ;;
      --all)
        all="true"
        shift
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        echo "unexpected ledger-summary argument: $1" >&2
        exit 2
        ;;
    esac
  done
  require_command jq
  if [[ "$all" == "true" ]]; then
    for account in $(known_accounts | sort -u); do
      run_ledger_summary_one "$account" "$date"
    done
  else
    run_ledger_summary_one "$account" "$date"
  fi
}

run_today() {
  local date fleet_json account path records candidates submit_results decisions reentry_blocks
  date="$(date +%F)"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --date)
        date="${2:-}"
        shift 2
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        echo "unexpected today argument: $1" >&2
        exit 2
        ;;
    esac
  done
  require_command jq
  require_executable "$FLEET_BIN" "fleet status binary"
  fleet_json="$(mktemp "${TMPDIR:-/tmp}/nautilus-alpaca-fleet.XXXXXX")"
  "$FLEET_BIN" --json > "$fleet_json"
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
  for account in $(known_accounts | sort -u); do
    path="$(candidate_ledger_file "$account" "$date")"
    if [[ -f "$path" ]]; then
      records="$(wc -l < "$path" | tr -d ' ')"
      candidates="$(ledger_count "$account" "$date" candidate)"
      submit_results="$(ledger_count "$account" "$date" submit_result)"
      decisions="$(ledger_count "$account" "$date" decision)"
      reentry_blocks="$(ledger_reentry_count "$account" "$date")"
      echo "ledger account=$account date=$date records=$records candidates=$candidates decisions=$decisions submit_results=$submit_results same_day_reentry_blocks=$reentry_blocks"
    else
      echo "ledger account=$account date=$date missing=true"
    fi
  done
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
  require_executable "$PERFORMANCE_BIN" "performance report binary"
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
  "$PERFORMANCE_BIN" "$@"
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
  run_performance --all --append-ledger --track-candidates --send-discord "$@"
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
  require_executable "$CANDIDATE_ALERTS_BIN" "candidate alerts binary"
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
  "$CANDIDATE_ALERTS_BIN" "$@"
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
    require_executable "$FLEET_BIN" "fleet status binary"
    "$FLEET_BIN" "$@"
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
  ledger-summary)
    run_ledger_summary "$@"
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
  scan)
    run_scan "$@"
    ;;
  run-once)
    run_once "$@"
    ;;
  ledger)
    run_ledger "$@"
    ;;
  *)
    usage
    exit 2
    ;;
esac
