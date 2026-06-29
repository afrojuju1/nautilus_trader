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
ENGINE_BIN="${NAUTILUS_ALPACA_RUNNER_BIN:-$HOME/.local/bin/alpaca-options-node}"
OPERATOR_BIN="${NAUTILUS_ALPACA_OPERATOR_BIN:-$HOME/.local/bin/alpaca-ops}"
FLEET_BIN="${NAUTILUS_ALPACA_FLEET_BIN:-$HOME/.local/bin/alpaca-ops}"
OPTION_CHAIN_LIVE_BIN="${NAUTILUS_ALPACA_OPTION_CHAIN_LIVE_BIN:-$HOME/.local/bin/alpaca-options-node}"
COMPARE_SCAN_BIN="${NAUTILUS_ALPACA_COMPARE_SCAN_BIN:-$HOME/.local/bin/alpaca-compare-option-chain-scan}"
CANDIDATE_ALERTS_BIN="${NAUTILUS_ALPACA_CANDIDATE_ALERTS_BIN:-$HOME/.local/bin/alpaca-ops}"
PERFORMANCE_BIN="${NAUTILUS_ALPACA_PERFORMANCE_BIN:-$HOME/.local/bin/alpaca-ops}"
ALERTS_ENV_FILE="${NAUTILUS_ALPACA_ALERTS_ENV_FILE:-$ALPACA_CONFIG_HOME/alerts.env}"
FLEET_CONFIG_FILE="${NAUTILUS_ALPACA_FLEET_CONFIG:-$ALPACA_CONFIG_HOME/fleet.toml}"
PROFILE_MANIFEST_FILE="${NAUTILUS_ALPACA_PROFILE_MANIFEST:-}"
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
  paper-defined-risk             shared SPY/QQQ defined-risk paper account
  paper-undefined-risk           undefined-risk paper account

Commands:
  accounts                       list known local account ids
  status                         systemd status plus operator summary for one account
  operator [ARGS...]             run alpaca-ops status for one account
  fleet [ARGS...]                run alpaca-ops fleet
  health                         lightweight service/account health check
  today                          compact fleet status
  strategy-report [ARGS...]      DB-backed strategy attribution and profile alignment report
  performance [--all] [ARGS...]  summarize candidate history and broker-fill PnL
  alerts candidates [ARGS...]    send or dry-run Discord candidate alerts from Postgres
  alerts performance [ARGS...]   send post-market Discord performance digest
  alerts enable|disable|status   control automatic Discord candidate alerts timer
  alerts performance-enable      enable automatic post-market performance digest timer
  alerts performance-disable     disable automatic post-market performance digest timer
  alerts performance-status      show post-market performance digest timer status
  check-config                   print resolved account config
  compare-scan [ARGS...]         compare REST scanner output with Nautilus option-chain output
  option-chain-live [ARGS...]    run the live Nautilus option-chain scan and entry-strategy node
  cutover-proof [ARGS...]        compare scan output, then run a bounded live node proof
  validate                       run Alpaca formatting, shell, test, and check commands
  deploy                         build and install local Alpaca runtime files
  rollout                        validate, deploy, restart all services, then summarize health
  start|stop|restart|logs        control one account's user service
  restart-all                    restart all known account services
  scan [PROFILE] [SYMBOLS]       one-shot dry-run candidate scan; disables submit/manage/close
  run-once [SYMBOLS]             one engine iteration using account runtime gates

Scan profiles:
  iron-condor, put-credit, call-credit, credit, directional, naked, naked-1-3dte

Examples:
  $(basename "$0") --account paper-undefined-risk check-config
  $(basename "$0") --account paper-defined-risk check-config
  $(basename "$0") --account paper-undefined-risk scan naked GDX,SLV
  $(basename "$0") --account paper-defined-risk scan credit SPY,QQQ
  $(basename "$0") strategy-report
  $(basename "$0") strategy-report --tomorrow
  $(basename "$0") today
  $(basename "$0") performance --all
  $(basename "$0") alerts candidates --all --dry-run
  $(basename "$0") alerts candidates --all --send
  $(basename "$0") alerts performance
  $(basename "$0") alerts enable
  $(basename "$0") alerts performance-enable
  $(basename "$0") compare-scan --pretty SPY 2026-07-02
  $(basename "$0") option-chain-live SPY 2026-07-02
  $(basename "$0") cutover-proof SPY 2026-07-02
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

profile_manifest_file() {
  if [[ -n "$PROFILE_MANIFEST_FILE" ]]; then
    printf '%s\n' "$PROFILE_MANIFEST_FILE"
  elif [[ -f "$ALPACA_CONFIG_HOME/paper-profiles.tsv" ]]; then
    printf '%s\n' "$ALPACA_CONFIG_HOME/paper-profiles.tsv"
  else
    printf '%s\n' "$REPO/deploy/alpaca/alpaca-paper-profiles.tsv"
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
  export NAUTILUS_ALPACA_OPTION_CHAIN_LIVE_BIN="$OPTION_CHAIN_LIVE_BIN"
  export NAUTILUS_ALPACA_COMPARE_SCAN_BIN="$COMPARE_SCAN_BIN"
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
  skip_keys=" ALPACA_SUBMIT ALPACA_MANAGE ALPACA_CLOSE ALPACA_KILL_SWITCH ALPACA_FORCE_FLATTEN ALPACA_CANCEL_AFTER_ACCEPT ALPACA_STRATEGY_FAMILIES ALPACA_DRY_RUN_FAMILIES ALPACA_MAX_ITERATIONS ALPACA_INTERVAL_SECS ALPACA_IGNORE_ENTRY_WINDOW ALPACA_MAX_ACTIVE_ENTRIES ALPACA_MAX_DAILY_SUBMITS ALPACA_MAX_OPEN_ORDERS ALPACA_MAX_ACTIVE_ENTRIES_PER_UNDERLYING ALPACA_MAX_ACTIVE_ENTRIES_PER_SECTOR NAUTILUS_ALPACA_ACCOUNT NAUTILUS_ALPACA_SERVICE NAUTILUS_ALPACA_REPO NAUTILUS_ALPACA_RUNNER_BIN NAUTILUS_ALPACA_OPERATOR_BIN NAUTILUS_ALPACA_LOG_DIR NAUTILUS_ALPACA_LOCK_DIR ALPACA_CONFIG_PATH "
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
    printf 'NAUTILUS_ALPACA_OPTION_CHAIN_LIVE_BIN=%s\n' "$NAUTILUS_ALPACA_OPTION_CHAIN_LIVE_BIN"
    printf 'NAUTILUS_ALPACA_COMPARE_SCAN_BIN=%s\n' "$NAUTILUS_ALPACA_COMPARE_SCAN_BIN"
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
    paper-defined-risk)
      printf '%s\n' "credit"
      ;;
    paper-put-credit-spy)
      printf '%s\n' "put-credit"
      ;;
    paper-call-credit-qqq)
      printf '%s\n' "call-credit"
      ;;
    paper-directional)
      printf '%s\n' "credit"
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
  "$OPERATOR_BIN" status "$@"
}

run_check_config() {
  local account
  account="$(normalize_account "$1")"
  setup_account_env "$account"
  require_executable "$ENGINE_BIN" "engine binary"
  "$ENGINE_BIN" --check-config
}

run_compare_scan() {
  local account
  account="$ACCOUNT"
  setup_account_env "$account"
  require_executable "$COMPARE_SCAN_BIN" "option-chain comparison binary"
  "$COMPARE_SCAN_BIN" "$@"
}

run_option_chain_live() {
  local account
  account="$ACCOUNT"
  setup_account_env "$account"
  require_executable "$OPTION_CHAIN_LIVE_BIN" "option-chain live node binary"
  "$OPTION_CHAIN_LIVE_BIN" "$@"
}

run_cutover_proof() {
  local account max_runtime
  account="$ACCOUNT"
  setup_account_env "$account"
  require_executable "$COMPARE_SCAN_BIN" "option-chain comparison binary"
  require_executable "$OPTION_CHAIN_LIVE_BIN" "option-chain live node binary"
  max_runtime="${ALPACA_OPTION_CHAIN_MAX_RUNTIME_SECS:-90}"
  echo "cutover_proof account=$(normalize_account "$account") phase=compare"
  "$COMPARE_SCAN_BIN" --pretty "$@"
  echo "cutover_proof account=$(normalize_account "$account") phase=live_node max_runtime_secs=$max_runtime"
  ALPACA_OPTION_CHAIN_MAX_RUNTIME_SECS="$max_runtime" "$OPTION_CHAIN_LIVE_BIN" "$@"
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
      "ALPACA_STRATEGY_FAMILIES=$strategies"
      "ALPACA_DRY_RUN_FAMILIES=$strategies"
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
      "ALPACA_STRATEGY_FAMILIES=$strategies" \
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
    overrides+=("ALPACA_STRATEGY_FAMILIES=$strategies")
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

run_today() {
  local fleet_json
  require_command jq
  require_executable "$FLEET_BIN" "fleet status binary"
  fleet_json="$(mktemp "${TMPDIR:-/tmp}/nautilus-alpaca-fleet.XXXXXX")"
  "$FLEET_BIN" fleet --json > "$fleet_json"
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
  "$PERFORMANCE_BIN" performance "$@"
}

env_value_from_file() {
  local file key
  file="$1"
  key="$2"
  [[ -f "$file" ]] || return 0
  awk -v key="$key" '
    /^[[:space:]]*($|#)/ { next }
    {
      line = $0
      sub(/^[[:space:]]*export[[:space:]]+/, "", line)
      if (line ~ "^[[:space:]]*" key "[[:space:]]*=") {
        sub(/^[^=]*=/, "", line)
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", line)
        if ((line ~ /^".*"$/) || (line ~ /^\047.*\047$/)) {
          line = substr(line, 2, length(line) - 2)
        }
        print line
        exit
      }
    }
  ' "$file"
}

report_env_value() {
  local account key account_env value
  account="$(normalize_account "$1")"
  key="$2"
  account_env="$(account_env_file "$account")"
  value="$(env_value_from_file "$account_env" "$key")"
  if [[ -z "$value" && "$account_env" != "$DEFAULT_ENV_FILE" ]]; then
    value="$(env_value_from_file "$DEFAULT_ENV_FILE" "$key")"
  fi
  printf '%s\n' "$value"
}

run_profile_alignment() {
  local account_filter manifest
  account_filter="$1"
  manifest="$(profile_manifest_file)"
  if [[ ! -f "$manifest" ]]; then
    echo "profile_alignment status=missing_manifest path=$manifest"
    return
  fi
  require_command python3
  python3 - "$manifest" "$ALPACA_CONFIG_HOME" "$ACCOUNT_CONFIG_DIR" "$REPO" "$account_filter" <<'PY'
import csv
import pathlib
import sys
import tomllib

manifest = pathlib.Path(sys.argv[1])
config_home = pathlib.Path(sys.argv[2])
account_config_dir = pathlib.Path(sys.argv[3])
repo = pathlib.Path(sys.argv[4])
account_filter = sys.argv[5]

def split_csv(value):
    value = (value or "").strip()
    if not value or value == "-":
        return []
    if value == "*":
        return ["*"]
    return [part.strip() for part in value.split(",") if part.strip()]

def expected_bool(value):
    value = (value or "").strip().lower()
    if value in ("true", "1", "yes", "on"):
        return True
    if value in ("false", "0", "no", "off"):
        return False
    return None

def field_path(config, section, field):
    return config.get(section, {}).get(field)

def compare(name, expected, actual, issues):
    if expected in ("", "-"):
        return
    if str(actual) != expected:
        issues.append(f"{name}:expected={expected}:actual={actual}")

def compare_list(name, expected_value, actual, issues):
    expected = split_csv(expected_value)
    if expected == ["*"] or not expected:
        return
    actual = actual or []
    if list(actual) != expected:
        issues.append(f"{name}:expected={','.join(expected)}:actual={','.join(map(str, actual))}")

def compare_bool(name, expected_value, actual, issues):
    expected = expected_bool(expected_value)
    if expected is None:
        return
    if bool(actual) != expected:
        issues.append(f"{name}:expected={str(expected).lower()}:actual={str(bool(actual)).lower()}")

def compare_float(name, expected_value, actual, issues):
    expected_value = (expected_value or "").strip()
    if not expected_value or expected_value == "-":
        return
    try:
        expected = float(expected_value)
        actual_float = float(actual)
    except (TypeError, ValueError):
        issues.append(f"{name}:expected={expected_value}:actual={actual}")
        return
    if abs(expected - actual_float) > 1e-9:
        issues.append(f"{name}:expected={expected_value}:actual={actual}")

def config_path_for(row):
    account = row["account_id"]
    if account == "paper-main":
        live = config_home / "options.toml"
    else:
        live = account_config_dir / f"{account}-options.toml"
    if live.exists():
        return live, "live"
    template = repo / "deploy" / "alpaca" / row["template"]
    return template, "template"

def merge_config(parent, child):
    merged = dict(parent)
    for key, value in child.items():
        if key == "extends":
            continue
        if isinstance(value, dict) and isinstance(merged.get(key), dict):
            merged[key] = merge_config(merged[key], value)
        else:
            merged[key] = value
    return merged

def load_config(path, seen=None):
    seen = seen or set()
    path = path.resolve()
    if path in seen:
        raise RuntimeError(f"circular config inheritance at {path}")
    seen.add(path)
    config = tomllib.loads(path.read_text())
    parent_ref = config.get("extends")
    if not parent_ref:
        return config
    parent_path = pathlib.Path(parent_ref)
    if not parent_path.is_absolute():
        parent_path = path.parent / parent_path
    parent = load_config(parent_path, seen)
    return merge_config(parent, config)

with manifest.open(newline="") as file:
    rows = list(csv.DictReader(file, delimiter="\t"))

for row in rows:
    account = row["account_id"]
    if account_filter and account != account_filter:
        continue
    path, source = config_path_for(row)
    if not path.exists():
        print(
            f"profile_alignment account={account} profile={row['profile_id']} "
            f"backtest={row['backtest_profile']} status=missing_config path={path}"
        )
        continue
    try:
        config = load_config(path)
    except Exception as exc:
        print(
            f"profile_alignment account={account} profile={row['profile_id']} "
            f"backtest={row['backtest_profile']} status=parse_error path={path} error={exc}"
        )
        continue

    issues = []
    compare_list(
        "strategy_families",
        row["strategy_families"],
        field_path(config, "runtime", "strategy_families"),
        issues,
    )
    compare_list(
        "dry_run_families",
        row["dry_run_families"],
        field_path(config, "runtime", "dry_run_families"),
        issues,
    )
    compare_bool("submit", row["submit"], field_path(config, "runtime", "submit"), issues)
    compare_bool("manage", row["manage"], field_path(config, "runtime", "manage"), issues)
    compare_bool("close", row["close"], field_path(config, "runtime", "close"), issues)
    compare_bool("kill_switch", row["kill_switch"], field_path(config, "runtime", "kill_switch"), issues)
    compare_list("underlyings", row["underlyings"], field_path(config, "universe", "underlyings"), issues)
    compare("entry_start", row["entry_start"], field_path(config, "universe", "entry_start"), issues)
    compare("entry_end", row["entry_end"], field_path(config, "universe", "entry_end"), issues)
    compare_float(
        "min_return_on_risk",
        row["min_return_on_risk"],
        field_path(config, "scanner", "min_return_on_risk"),
        issues,
    )
    for field in (
        "max_active_entries",
        "max_daily_submits",
        "max_open_orders",
        "max_active_entries_per_underlying",
        "max_active_entries_per_sector",
    ):
        compare(field, row[field], field_path(config, "risk", field), issues)

    runtime = config.get("runtime", {})
    universe = config.get("universe", {})
    status = "ok" if not issues else "mismatch"
    issue_text = "none" if not issues else ";".join(issues)
    gates = (
        f"submit={runtime.get('submit')} manage={runtime.get('manage')} "
        f"close={runtime.get('close')} kill_switch={runtime.get('kill_switch')}"
    )
    window = f"{universe.get('entry_start', '-')}-{universe.get('entry_end', '-')}"
    print(
        f"profile_alignment account={account} profile={row['profile_id']} "
        f"backtest={row['backtest_profile']} status={status} source={source} "
        f"window={window} {gates} issues={issue_text}"
    )
PY
}

run_strategy_report_sql() {
  local account_filter since until database_url schema db_container db_user db_name
  local -a psql_cmd
  account_filter="$1"
  since="$2"
  until="$3"
  database_url="$4"
  schema="$5"
  if command -v psql >/dev/null 2>&1; then
    psql_cmd=(psql -X -q -v ON_ERROR_STOP=1 -v schema="$schema" -v since="$since" -v until="$until" -v account="$account_filter" "$database_url")
  elif command -v docker >/dev/null 2>&1; then
    db_container="${NAUTILUS_ALPACA_DB_CONTAINER:-nautilus-database}"
    if docker ps --format '{{.Names}}' | grep -qx "$db_container"; then
      db_user="${NAUTILUS_ALPACA_DB_USER:-nautilus}"
      db_name="${NAUTILUS_ALPACA_DB_NAME:-nautilus}"
      psql_cmd=(docker exec -i "$db_container" psql -U "$db_user" -d "$db_name" -X -q -v ON_ERROR_STOP=1 -v schema="$schema" -v since="$since" -v until="$until" -v account="$account_filter")
    else
      echo "strategy_report storage=unavailable reason=missing_psql_and_container container=$db_container" >&2
      return 127
    fi
  else
    echo "strategy_report storage=unavailable reason=missing_psql_and_docker" >&2
    return 127
  fi
  "${psql_cmd[@]}" <<'SQL'
\pset pager off
\pset tuples_only on
\pset format unaligned
\pset fieldsep ' | '
\echo strategy_activity
SELECT
  account_id,
  trade_date,
  COALESCE(NULLIF(payload->>'strategy', ''), 'all') AS strategy,
  COUNT(*) FILTER (WHERE record_type = 'candidate') AS candidates,
  COUNT(*) FILTER (WHERE record_type = 'candidate_alert' AND alert_type = 'high_score_candidate') AS high_score,
  COUNT(*) FILTER (WHERE record_type = 'candidate_alert' AND alert_type = 'selected_candidate') AS selected,
  COUNT(*) FILTER (WHERE record_type = 'decision') AS decisions,
  COUNT(*) FILTER (WHERE record_type = 'submit_result') AS submit_results,
  COALESCE(SUM(
    CASE
      WHEN record_type = 'submit_result' AND COALESCE(payload->>'accepted', '') ~ '^[0-9]+$'
        THEN (payload->>'accepted')::integer
      ELSE 0
    END
  ), 0) AS accepted,
  COALESCE(SUM(
    CASE
      WHEN record_type = 'submit_result' AND COALESCE(payload->>'rejected', '') ~ '^[0-9]+$'
        THEN (payload->>'rejected')::integer
      ELSE 0
    END
  ), 0) AS rejected
FROM :"schema".candidate_ledger
WHERE trade_date >= :'since'::date
  AND trade_date <= :'until'::date
  AND (:'account' = '' OR account_id = :'account')
GROUP BY account_id, trade_date, COALESCE(NULLIF(payload->>'strategy', ''), 'all')
ORDER BY account_id, trade_date, strategy;
\echo blocked_or_skipped_decisions
SELECT
  account_id,
  trade_date,
  COALESCE(NULLIF(payload->>'strategy', ''), 'all') AS strategy,
  COALESCE(NULLIF(payload->>'action', ''), 'unknown') AS action,
  COALESCE(NULLIF(payload->>'reason', ''), 'none') AS reason,
  COUNT(*) AS records
FROM :"schema".candidate_ledger
WHERE trade_date >= :'since'::date
  AND trade_date <= :'until'::date
  AND record_type = 'decision'
  AND (:'account' = '' OR account_id = :'account')
GROUP BY account_id, trade_date, strategy, action, reason
ORDER BY account_id, trade_date, records DESC, strategy, action, reason;
\echo closed_performance
SELECT
  account_id,
  ledger_date,
  COALESCE(NULLIF(payload->>'strategy', ''), 'unknown') AS strategy,
  COUNT(*) AS closed_trades,
  COALESCE(SUM((payload->>'realized_pnl')::numeric), 0) AS realized_pnl,
  COUNT(*) FILTER (WHERE (payload->>'realized_pnl')::numeric > 0) AS wins,
  COUNT(*) FILTER (WHERE (payload->>'realized_pnl')::numeric < 0) AS losses,
  COUNT(*) FILTER (WHERE (payload->>'realized_pnl')::numeric = 0) AS flats
FROM :"schema".performance_ledger
WHERE ledger_date >= :'since'::date
  AND ledger_date <= :'until'::date
  AND (payload->>'type') = 'realized_trade'
  AND (:'account' = '' OR account_id = :'account')
GROUP BY account_id, ledger_date, strategy
ORDER BY account_id, ledger_date, strategy;
\echo open_positions
SELECT
  state.account_id,
  COALESCE(NULLIF(entry->>'strategy', ''), 'unknown') AS strategy,
  COALESCE(NULLIF(entry->>'underlying', ''), 'unknown') AS underlying,
  COUNT(*) AS active_entries,
  COALESCE(MIN(entry->>'trade_date'), '-') AS oldest_trade_date,
  COALESCE(MAX(entry->>'score'), '-') AS max_score
FROM :"schema".strategy_state AS state
CROSS JOIN LATERAL jsonb_array_elements(state.state::jsonb->'entries') AS entry
WHERE COALESCE((entry->>'submitted')::boolean, false)
  AND NOT COALESCE((entry->>'canceled')::boolean, false)
  AND NOT COALESCE((entry->>'closed')::boolean, false)
  AND (:'account' = '' OR state.account_id = :'account')
GROUP BY state.account_id, strategy, underlying
ORDER BY state.account_id, strategy, underlying;
\echo latest_decisions
SELECT DISTINCT ON (account_id)
  account_id,
  ts_utc,
  COALESCE(NULLIF(payload->>'action', ''), 'unknown') AS action,
  COALESCE(NULLIF(payload->>'reason', ''), 'none') AS reason,
  COALESCE(NULLIF(payload->>'strategy', ''), 'all') AS strategy,
  COALESCE(NULLIF(payload->>'underlying', ''), '-') AS underlying
FROM :"schema".candidate_ledger
WHERE record_type = 'decision'
  AND trade_date >= :'since'::date
  AND trade_date <= :'until'::date
  AND (:'account' = '' OR account_id = :'account')
ORDER BY account_id, ts_utc DESC;
\echo entry_window_validation_0945_1015_et
SELECT
  account_id,
  trade_date,
  COUNT(*) FILTER (WHERE record_type = 'scan_started') AS scans,
  COUNT(*) FILTER (WHERE record_type = 'scanner_result') AS scanner_results,
  COUNT(*) FILTER (WHERE record_type = 'candidate') AS candidates,
  COUNT(*) FILTER (WHERE record_type = 'candidate_alert' AND alert_type = 'selected_candidate') AS selected,
  COUNT(*) FILTER (WHERE record_type = 'submit_result') AS submit_results,
  COUNT(*) FILTER (WHERE record_type = 'decision' AND payload->>'action' = 'selected_but_blocked') AS blocked,
  COUNT(*) FILTER (WHERE record_type = 'decision' AND payload->>'action' = 'skipped') AS skipped
FROM :"schema".candidate_ledger
WHERE trade_date >= :'since'::date
  AND trade_date <= :'until'::date
  AND ((ts_utc AT TIME ZONE 'America/New_York')::time >= TIME '09:45')
  AND ((ts_utc AT TIME ZONE 'America/New_York')::time <= TIME '10:15')
  AND (:'account' = '' OR account_id = :'account')
GROUP BY account_id, trade_date
ORDER BY account_id, trade_date;
SQL
}

run_strategy_report() {
  local account since until database_account database_url schema
  account=""
  since="$(date +%F)"
  until="$since"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --account|-a)
        account="$(normalize_account "${2:-}")"
        shift 2
        ;;
      --all)
        account=""
        shift
        ;;
      --date)
        since="${2:-}"
        until="$since"
        shift 2
        ;;
      --tomorrow)
        since="$(date -d tomorrow +%F)"
        until="$since"
        shift
        ;;
      --since)
        since="${2:-}"
        shift 2
        ;;
      --until)
        until="${2:-}"
        shift 2
        ;;
      --help|-h)
        echo "usage: $(basename "$0") strategy-report [--all] [--account ACCOUNT] [--date YYYY-MM-DD|--tomorrow|--since YYYY-MM-DD --until YYYY-MM-DD]" >&2
        return 0
        ;;
      *)
        echo "unexpected strategy-report argument: $1" >&2
        exit 2
        ;;
    esac
  done
  until="${until:-$since}"
  echo "strategy_report since=$since until=$until account=${account:-all}"
  echo "profile_alignment"
  run_profile_alignment "$account"
  database_account="${account:-paper-main}"
  database_url="$(report_env_value "$database_account" "ALPACA_STORAGE_DATABASE_URL")"
  schema="$(report_env_value "$database_account" "ALPACA_STORAGE_SCHEMA")"
  schema="${schema:-alpaca}"
  if [[ -z "$database_url" ]]; then
    echo "strategy_report storage=missing key=ALPACA_STORAGE_DATABASE_URL account=$database_account"
    return
  fi
  run_strategy_report_sql "$account" "$since" "$until" "$database_url" "$schema"
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
  "$CANDIDATE_ALERTS_BIN" alerts candidates "$@"
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
    "$FLEET_BIN" fleet "$@"
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
  strategy-report)
    run_strategy_report "$@"
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
  compare-scan)
    run_compare_scan "$@"
    ;;
  option-chain-live)
    run_option_chain_live "$@"
    ;;
  cutover-proof)
    run_cutover_proof "$@"
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
  *)
    usage
    exit 2
    ;;
esac
