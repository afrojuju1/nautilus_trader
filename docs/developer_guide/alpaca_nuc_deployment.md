# Alpaca NUC Deployment

This deployment runs the Rust Alpaca options runner as one supervised user service for the
active account. It keeps credentials in an untracked repo-local `.env`, writes logs under the
user's state directory, and uses a file lock so only one runner can own a given Alpaca account
workflow.

## Files

- `deploy/alpaca/alpaca-options.env.example`: credentials, endpoints, service paths, and
  emergency override template.
- `deploy/alpaca/alpaca-options.account.env.example`: optional account-scoped override env
  template for supervised account instances that need a separate boundary.
- `deploy/alpaca/alpaca-paper-profiles.tsv`: checked profile manifest linking paper accounts,
  runtime config expectations, and backtest profile IDs.
- `deploy/alpaca/alpaca-options.base.toml.example`: shared strategy universe, scanner, sector,
  and management defaults inherited by account configs.
- `deploy/alpaca/alpaca-options.toml.example`: strategy/scanner/management config template.
- `deploy/alpaca/alpaca-options.paper-directional.toml.example`: defined-risk watchlist
  account config template.
- `deploy/alpaca/alpaca-options.paper-put-credit-spy.toml.example`: isolated SPY
  put-credit account config template.
- `deploy/alpaca/alpaca-options.paper-call-credit-qqq.toml.example`: isolated QQQ
  call-credit account config template.
- `deploy/alpaca/alpaca-options.paper-undefined-risk.toml.example`: isolated undefined-risk
  account config template.
- `deploy/alpaca/alpaca-fleet.toml.example`: read-only fleet registry template.
- `deploy/alpaca/alpaca-options-install.sh`: builds release binaries and installs user files.
- `deploy/alpaca/alpaca-options-runner.sh`: `flock`-guarded runner wrapper.
- `deploy/alpaca/alpaca-options.service`: user systemd service.
- `deploy/alpaca/alpaca-options@.service`: disabled-by-default account-instance service
  template.
- `deploy/alpaca/alpaca-control.sh`: account-aware operator status, config checks, candidate
  scans, ledger summaries, health, validation/deploy rollout helpers, service control, fleet
  status, and logs.
- `deploy/alpaca/alpaca-options.logrotate`: optional logrotate policy.

## Install

From the repo root:

```bash
deploy/alpaca/alpaca-options-install.sh
```

Edit the repo-local `.env` and add Alpaca paper credentials. Keep `ALPACA_OPEN_ORDERS=false` and
`ALPACA_CLOSE_ORDERS=false` until paper proof is intentionally enabled.
Set `NAUTILUS_ALPACA_ACCOUNT` or an account-specific systemd instance when running against the
fleet registry; Docker uses `NAUTILUS_ALPACA_DOCKER_ACCOUNT` from the repo `.env` or shell and does
not assume a fleet account when that value is omitted.

The installed engine, operator status command, and account/order probe auto-load the repo `.env`
when `NAUTILUS_ALPACA_REPO` points at the checkout or when commands run from the repo tree. Set
`NAUTILUS_ALPACA_ENV_FILE` only when intentionally pointing at a different env file for an account
or diagnostic boundary.

Edit `~/.config/nautilus-trader/alpaca/base-options.toml` for shared scanner, universe,
sector, and management settings. Account configs can set top-level `extends` to inherit from that
base, then override only strategy identity, state path, risk caps, or account-specific scanner
limits. TOML `runtime.max_iterations = 0` is continuous service mode. Set
`ALPACA_MAX_ITERATIONS=1` only for a manual one-shot smoke test override.

Candidate-ledger evidence is enabled by default with `runtime.candidate_ledger_enabled = true`.
Live runtime state, candidate ledgers, performance ledgers, and candidate outcomes are stored in
Postgres through `NAUTILUS_OPERATIONAL_DATABASE_URL`.
Strategy spread identity is stored directly in normalized `strategy_state` rows, with account
metadata in `strategy_state_account` and separate broker leg/order evidence in
`strategy_broker_leg_evidence`. State mutations are recorded in `strategy_state_events`. The local
JSON state file is a recovery mirror, not the primary operator source. Use
`docs/developer_guide/alpaca_operational_state_runbook.md` before applying or restoring
operational-state migrations.
`runtime.candidate_ledger_max_candidates` controls how many ranked candidates per scanner result
are persisted; `0` records all ranked candidates.

Candidate Discord alerts are emitted by a sidecar command which consumes typed `candidate_alert`
records from the Postgres candidate ledger, not by the trading loop. Put the webhook in
`~/.config/nautilus-trader/alpaca/alerts.env` as `DISCORD_WEBHOOK_URL=...`; keep the file mode at
`600`.

The installer also creates `~/.config/nautilus-trader/alpaca/fleet.toml` if missing. The fleet
registry is read-only operator metadata; credentials remain in the shared untracked repo `.env`
unless an account intentionally opts into its own env file.

## Configuration Layering and Overrides

The deployed runtime has three configuration layers:

1. The shared repo `.env` carries credentials, endpoints, installed binary paths, account identity,
   and emergency runtime gates. Account env files are optional explicit boundaries.
2. TOML files carry strategy, scanner, universe, risk, management, state, and ledger settings.
3. The fleet registry carries account roles, permissions, risk budgets, service names, and
   per-account file paths.

The options engine resolves the TOML config in this order:

- `ALPACA_CONFIG_PATH`, when set.
- The current fleet account's `config_file`, when the account can be matched from
  `NAUTILUS_ALPACA_ACCOUNT`, `NAUTILUS_ALPACA_SERVICE`, `ALPACA_CONFIG_PATH`, or
  `NAUTILUS_ALPACA_ENV_FILE`.
- `~/.config/nautilus-trader/alpaca/options.toml`, when present.
- Built-in runtime defaults.

TOML `extends` loads the parent file first and then applies the child file. Child scalar values
override parent scalar values, non-empty child lists override parent lists, and maps such as
`[risk.sectors]` are merged with child keys replacing parent keys. Keep shared scanner, universe,
sector, and management defaults in `base-options.toml`; keep account identity, state paths,
and account-level caps in the account config.

Environment overrides are intentionally narrow and operational. They take precedence over TOML for
the fields they support:

- Runtime controls: `ALPACA_MAX_ITERATIONS`, `ALPACA_INTERVAL_SECS`,
  `ALPACA_IGNORE_ENTRY_WINDOW`. Strategy composition, per-profile quantity, scanner thresholds,
  and normal risk settings live in `ALPACA_CONFIG_PATH` as `[[strategies]]` blocks.
- Order capability gates: `ALPACA_OPEN_ORDERS`, `ALPACA_CLOSE_ORDERS`,
  `ALPACA_FORCE_FLATTEN`, `ALPACA_CANCEL_AFTER_ACCEPT`.
- Account caps: `ALPACA_MAX_ACTIVE_ENTRIES`, `ALPACA_MAX_DAILY_SUBMITS`,
  `ALPACA_MAX_OPEN_ORDERS`, `ALPACA_MAX_ACTIVE_ENTRIES_PER_UNDERLYING`,
  `ALPACA_MAX_ACTIVE_ENTRIES_PER_SECTOR`.
- Close management: `ALPACA_CLOSE_REGULAR_HOURS_ONLY`, `ALPACA_CLOSE_PRICE_CUSHION`,
  `ALPACA_MAX_CLOSE_ATTEMPTS`, `ALPACA_CLOSE_REPRICE_COOLDOWN_SECS`.
- Paths: `ALPACA_CONFIG_PATH`, `ALPACA_STATE_PATH`, `ALPACA_PERFORMANCE_LEDGER_DIR`.

Not every TOML field has an env override. Scanner thresholds, sector maps, and most management
parameters should stay in TOML so the checked config remains reviewable.

Env file loading differs by intent. If `NAUTILUS_ALPACA_ENV_FILE` is unset, the runtime auto-loads
the repo `.env` from `NAUTILUS_ALPACA_REPO` or by walking up from the current directory, while
preserving values already in the process environment. If `NAUTILUS_ALPACA_ENV_FILE` is set, that
file is an explicit account boundary: keys declared in the file override inherited values, and a
missing file is an error. Fleet status uses an even stricter account boundary by clearing inherited
`ALPACA_*` and `NAUTILUS_ALPACA_*` values before launching each per-account operator-status child.

Fleet policy is applied after TOML and env resolution. Disabled accounts, role/permission
mismatches, or fleet kill-switch activation force `open_orders_enabled=false`.
Fleet account risk budgets can also tighten account limits; they should not be used to loosen the
account TOML.

## Commands

```bash
alpaca-control accounts
alpaca-control today
alpaca-control --account paper-main status
alpaca-control --account paper-directional status
alpaca-control --account paper-put-credit-spy status
alpaca-control --account paper-call-credit-qqq status
alpaca-control --account paper-undefined-risk status
alpaca-control --account paper-undefined-risk check-config
alpaca-control alerts candidates --all --dry-run
alpaca-control alerts candidates --all --send
alpaca-control alerts enable
alpaca-control alerts status
alpaca-control performance --all
alpaca-control performance --all --json
alpaca-control alerts performance
alpaca-control alerts performance-enable
alpaca-control alerts performance-status
alpaca-control validate
alpaca-control deploy
alpaca-control rollout
alpaca-control fleet --json
alpaca-control health
alpaca-control logs
nautilus adapters alpaca fleet --json
nautilus ops status --account-id paper-main
```

## Dockerized Local Bring-Up

The repo also provides a Docker Compose wrapper for the Alpaca Rust runtime at
`deploy/alpaca/compose.yml`. It is intentionally split into profiles so `docker compose up` starts
only Postgres. Read-only account checks and the trading engine require explicit service/profile
selection.

Start the Compose-local Postgres used by the containerized runtime:

```bash
docker compose --env-file .env -f deploy/alpaca/compose.yml up -d postgres
```

Avoid running `docker compose config` with the real repo `.env` because Compose prints resolved
environment values. Validate the Compose shape without `--env-file`, or use the checked
`alpaca-options.docker.env.example` template.

The default Docker build is optimized for local rollouts: it uses the `release-rollout` Cargo
profile and the normal runtime image includes only the engine, operator status, account check, and
live option-chain smoke binary. Compose services that require fleet status, compare scan, candidate
alerts, or performance report tooling build `nautilus-alpaca-tools:local` separately. Set
`ALPACA_DOCKER_BUILD_TOOLS=true` only when the normal runtime image must also carry those optional
tools. Set `ALPACA_DOCKER_CARGO_PROFILE=release` for a production-style release build:

```bash
ALPACA_DOCKER_CARGO_PROFILE=release \
ALPACA_DOCKER_BUILD_TOOLS=true \
docker compose \
  --env-file .env \
  -f deploy/alpaca/compose.yml \
  --profile engine \
  build alpaca-options
```

If local TOML configs are mode-restricted, stage container-readable copies outside the repo before
starting the service. Credentials stay in the repo `.env` and are not copied:

```bash
install -d -m 755 ~/.local/share/nautilus-alpaca-docker-config
install -m 644 ~/.config/nautilus-trader/alpaca/base-options.toml \
  ~/.local/share/nautilus-alpaca-docker-config/base-options.toml
install -m 644 ~/.config/nautilus-trader/alpaca/options.toml \
  ~/.local/share/nautilus-alpaca-docker-config/options.toml
```

Run read-only Alpaca checks with paper credentials from the repo `.env`:

```bash
docker compose \
  --env-file .env \
  -f deploy/alpaca/compose.yml \
  run --rm alpaca-check-account

docker compose \
  --env-file .env \
  -f deploy/alpaca/compose.yml \
  run --rm alpaca-status
```

When Docker owns the runtime, prefer the Docker status commands above plus
`docker compose --env-file .env -f deploy/alpaca/compose.yml --profile engine ps`. The
`alpaca-control fleet` command is systemd-oriented and reports systemd account services as inactive
when Docker is the active owner.

The Docker defaults keep `ALPACA_OPEN_ORDERS=false` and `ALPACA_CLOSE_ORDERS=false`. To run the
actual containerized engine, make the paper-trading intent explicit in the repo `.env` or shell,
choose the fleet account, point the Docker config mounts at reviewed local configs, keep paper
endpoints in place, and start only the engine profile:

```bash
NAUTILUS_ALPACA_DOCKER_ACCOUNT=paper-put-credit-spy \
NAUTILUS_ALPACA_DOCKER_BASE_CONFIG=~/.local/share/nautilus-alpaca-docker-config/base-options.toml \
NAUTILUS_ALPACA_DOCKER_CONFIG=~/.local/share/nautilus-alpaca-docker-config/options.toml \
NAUTILUS_ALPACA_DOCKER_OPEN_ORDERS=true \
NAUTILUS_ALPACA_DOCKER_CLOSE_ORDERS=true \
docker compose \
  --env-file .env \
  -f deploy/alpaca/compose.yml \
  --profile engine \
  up -d alpaca-options
```

Stop the containerized engine without removing Postgres data:

```bash
docker compose --env-file .env -f deploy/alpaca/compose.yml --profile engine stop alpaca-options
```

Ad hoc candidate scans and cutover proofs are no longer `alpaca-control` commands. Use the
Nautilus node, explicit cargo diagnostics, or the documented Docker cutover profiles so deployment
control stays separate from migration tooling.

Paper smoke tests that submit orders must be deliberate and short-lived. Keep paper endpoints in
the repo `.env`, set `ALPACA_MAX_ITERATIONS=1`, keep quantity at `1`, and set
`ALPACA_CANCEL_AFTER_ACCEPT=true` only for the smoke run. After the run, check account state with
`alpaca-control --account <account> status`; if any accepted smoke order remains open, cancel it in
the Alpaca paper dashboard or API before continuing. Do not leave accepted smoke orders working
unless the test explicitly requires it.

To enable start on user login:

```bash
systemctl --user enable alpaca-options.service
loginctl enable-linger "$USER"
```

For direct service control:

```bash
systemctl --user start alpaca-options.service
systemctl --user status alpaca-options.service --no-pager
systemctl --user restart alpaca-options.service
systemctl --user stop alpaca-options.service
journalctl --user -u alpaca-options.service -f
```

For account-instance services:

```bash
systemctl --user start alpaca-options@paper-directional.service
systemctl --user status alpaca-options@paper-directional.service --no-pager
systemctl --user start alpaca-options@paper-put-credit-spy.service
systemctl --user start alpaca-options@paper-call-credit-qqq.service
systemctl --user restart alpaca-options@paper-undefined-risk.service
systemctl --user stop alpaca-options@paper-undefined-risk.service
```

The same controls are available through the account-aware wrapper:

```bash
alpaca-control --account paper-main start
alpaca-control --account paper-main status
alpaca-control --account paper-main restart
alpaca-control --account paper-main stop
alpaca-control --account paper-main logs
```

## Runtime State

Default state files from the env template:

- Logs: `~/.local/state/nautilus_trader/logs/alpaca-options.log`
- Lock: `~/.local/state/nautilus_trader/locks/alpaca-options.lock`
- Strategy-state mirror: `~/.local/state/nautilus_trader/alpaca_options_state.json`
- Spread intents: Postgres `trading_ops.strategy_state`
- Strategy-state account metadata: Postgres `trading_ops.strategy_state_account`
- Broker leg evidence: Postgres `trading_ops.strategy_broker_leg_evidence`
- Candidate ledger: Postgres `trading_ops.candidate_ledger`
- Strategy-state events: Postgres `trading_ops.strategy_state_events`
- Runtime lease: Postgres `trading_ops.runtime_lease`
- Candidate-alert dedupe state:
  `~/.local/state/nautilus_trader/alpaca/<account-id>/alerts/candidate-discord-state.json`
- Performance ledger: Postgres `trading_ops.performance_ledger`
- Candidate outcomes: Postgres `trading_ops.candidate_outcome`

The runner wrapper takes an exclusive non-blocking lock. If another process already owns the lock,
the service exits without starting another Alpaca account owner.

The service runs installed release binaries by default:

- Runner: `~/.local/bin/alpaca-options-node`
- Operator status: `~/.local/bin/nautilus adapters alpaca status`
- Fleet status: `~/.local/bin/nautilus adapters alpaca fleet`

Override with `NAUTILUS_ALPACA_RUNNER_BIN` or `NAUTILUS_ALPACA_OPERATOR_BIN` only for diagnostics.

Account-scoped services use template-owned log and lock directories, for example
`~/.local/state/nautilus_trader/alpaca/<account-id>/logs` and
`~/.local/state/nautilus_trader/alpaca/<account-id>/locks`. Keep each account's strategy state path
inside its account config so one account cannot read or write another account's runtime state.

Ledger meanings:

- Strategy state is the engine's mutable local record of accepted entries, close attempts, close
  status, and strategy metadata.
- Candidate-ledger files are append-only scan evidence. Record types include `scanner_result`,
  `candidate`, `candidate_alert`, `decision`, and `submit_result`.
- Candidate-alert state is only a Discord dedupe file; deleting it can resend still-eligible
  ledger alerts.
- Performance-ledger files are append-only realized close records. Records are deduped by a stable
  close key and store reconstructed open cashflow, close cashflow, realized PnL, warnings, and the
  associated strategy entry.
- Candidate-outcome files are historical opportunity observations produced by the performance
  report. They value past scanner candidates at later observation buckets without implying the
  candidate was traded.

## Multi-Account Foundation

The existing paper account remains `alpaca-options.service` and continues to use:

- Env: repo `.env`
- Config: `~/.config/nautilus-trader/alpaca/options.toml`

Additional accounts are represented in `~/.config/nautilus-trader/alpaca/fleet.toml` with explicit
roles, permissions, risk budgets, service names, optional env-file overrides, config files, state
files, log directories, and lock directories. The account engine reads this registry on startup to
enforce account permissions, the fleet kill switch, and fleet-level active-entry caps before
submitting new entries.

The example fleet separates paper account roles deliberately:

| Account | Service | Role | Default state | Permission boundary |
|---------|---------|------|---------------|---------------------|
| `paper-main` | `alpaca-options.service` | `defined_risk_short_premium` | Enabled | Iron-condor watchlist/management only by default. |
| `paper-directional` | `alpaca-options@paper-directional.service` | `defined_risk_watchlist` | Enabled | Put-credit and call-credit watchlist scans only. |
| `paper-put-credit-spy` | `alpaca-options@paper-put-credit-spy.service` | `defined_risk_put_credit_spy` | Enabled | Isolated SPY put-credit paper profile. |
| `paper-call-credit-qqq` | `alpaca-options@paper-call-credit-qqq.service` | `defined_risk_call_credit_qqq` | Enabled | Isolated QQQ call-credit paper profile. |
| `paper-undefined-risk` | `alpaca-options@paper-undefined-risk.service` | `undefined_risk_short_premium` | Enabled | Naked calls and naked puts only, behind explicit undefined-risk permissions. |

Do not mix these roles casually. A strategy/role mismatch forces submission off and the kill switch
on for that account runtime, but the operator should still keep each account's TOML, state path, log
directory, lock directory, and risk budget separate.

For account-instance services, the systemd template owns account identity, service name, config
path, log path, and lock path. Use the repo-local `.env` as the shared credentials, endpoints, and
safety-gate source unless an account intentionally needs a separate boundary. Fleet status creates a
hermetic operator-status child process for each account instead of inheriting any `ALPACA_*` or
`NAUTILUS_ALPACA_*` values from the parent shell.

The hosted strategy names are:

- `put`, `put_credit`: put-credit verticals.
- `call`, `call_credit`: call-credit verticals.
- `iron_condor`, `condor`: four-leg iron condors.
- `call_debit`: long call-debit verticals.
- `put_debit`: long put-debit verticals.
- `debit`, `long_premium`, `directional`: both call-debit and put-debit verticals.
- `naked_call`, `naked_put`: undefined-risk short options.
- `naked_call_1_3dte`, `naked_put_1_3dte`: short-DTE undefined-risk profile.

Accounts with credit verticals or iron condors require `defined_risk = true` in the fleet registry.
Accounts with debit verticals require `long_premium = true`. A mismatch forces
`open_orders_enabled=false` for that runtime while leaving close-order capability available for
existing tracked exposure.

The paper profile manifest documents account intent, but the shell wrapper no longer owns
DB-backed strategy attribution. Use `nautilus adapters alpaca performance` for Alpaca
broker-backed performance reads and promote source-neutral attribution into `nautilus ops` when the
read model no longer depends on Alpaca broker fills.

Only create an account env file when an account intentionally needs credentials, endpoints, or
safety gates that differ from the shared repo `.env`.

Future account configs should live under:

```text
~/.config/nautilus-trader/alpaca/configs/<account-id>-options.toml
```

Keep extra accounts disabled in the fleet registry and keep their env gates inert until credentials,
permissions, strategy config, and risk budget are reviewed:

```bash
ALPACA_OPEN_ORDERS=false
ALPACA_CLOSE_ORDERS=false
```

After an account config and any env override are reviewed, the account can be enabled explicitly:

```bash
systemctl --user start alpaca-options@paper-directional.service
systemctl --user start alpaca-options@paper-put-credit-spy.service
systemctl --user start alpaca-options@paper-call-credit-qqq.service
```

Do not enable new account-instance services on login until paper proof is complete for that
account's role and strategy set.

## Order Capability Gates

- `ALPACA_OPEN_ORDERS=true` allows broker orders that open new risk.
- `ALPACA_CLOSE_ORDERS=true` allows broker orders that close or reduce risk, including stale-order
  cancellation and close submissions.
- `ALPACA_FORCE_FLATTEN=true` treats every tracked open spread as a close candidate.
- TOML `management.close_order_mode = "legacy_leg_order_list"` keeps close submission on the
  established leg order-list path.
- TOML `management.close_order_mode = "option_spread"` submits one reduce-only Nautilus
  `OptionSpread` close order expanded to Alpaca MLeg, and should be enabled only in a reviewed
  vertical paper profile.
- TOML strategy profile `mode = "dry_run"` lets a strategy scan and emit decisions without
  submitting while other enabled profiles can remain live.
- TOML `management.close_regular_hours_only = true` blocks non-forced close submissions outside the
  configured close window. `ALPACA_FORCE_FLATTEN=true` bypasses this guard for explicit flattening.
- TOML `management.stale_close_secs` controls close-order cancel/reprice timing separately from
  stale entry cancellation.
- TOML `management.close_price_cushion` adds an explicit debit cushion to close limits to reduce
  parked close orders.
- TOML `management.max_close_attempts` caps accepted close submissions per entry. Set `0` only for
  unlimited reprice attempts.
- TOML `management.close_reprice_cooldown_secs` delays resubmission after a close attempt.
- TOML `management.expiration_exit_days` applies to credit and debit spreads so near-expiration
  tracked exposure is treated as a close candidate.
- TOML `[scanner] min_credit_to_width` and `[debit_scanner] min_debit_to_width` reject underpaid
  spread candidates before ranking. Candidate ranking also favors centered DTE and stronger
  minimum-leg open interest.
- TOML `[naked_scanner]` controls undefined-risk single-leg short option candidates. It filters by
  DTE, absolute delta, open interest, bid/ask spread, minimum credit, displayed quote size, option
  volume, implied-volatility range, annualized premium yield, breakeven POP, estimated touch
  probability, spot-to-breakeven distance, expected-move coverage, estimated buying-power usage,
  return on estimated buying-power requirement, and composite candidate score before the
  account-level fleet permission gate can allow any naked-call or naked-put submission.
- The deployed undefined-risk paper profile is intentionally short-DTE focused (`3-7` DTE) so the
  annualized-yield gate can find liquid candidates without reaching too close to the money.
- TOML `[naked_1_3dte_scanner]` controls the optional `naked_call_1_3dte` and
  `naked_put_1_3dte` strategies. This profile is stricter on POP, touch risk, expected-move
  coverage, and liquidity because near-expiration short options have faster gamma changes.
- Naked-option scanner diagnostics include `account_options_buying_power`,
  `estimated_buying_power_requirement`, `buying_power_usage_pct`, `return_on_buying_power`, and the
  `capital_requirement_model`. Short puts use a cash-secured reserve estimate; short calls use an
  estimated Reg-T style requirement because max loss is undefined.
- TOML `[risk] max_active_entries`, `max_daily_submits`, and `max_open_orders` cap account-level
  exposure before any hosted strategy can submit. Env overrides are available as
  `ALPACA_MAX_ACTIVE_ENTRIES`, `ALPACA_MAX_DAILY_SUBMITS`, and `ALPACA_MAX_OPEN_ORDERS`.
- The entry scanner blocks same-day re-entry for an underlying after any accepted entry submission,
  including entries later closed by stop-loss or marked canceled. This prevents intraday churn back
  into the same name after a losing close.
- TOML `[risk] max_active_entries_per_underlying`, `max_active_entries_per_sector`, and
  `[risk.sectors]` cap concentration by symbol and configured correlation group. Env overrides are
  available for the two numeric caps as `ALPACA_MAX_ACTIVE_ENTRIES_PER_UNDERLYING` and
  `ALPACA_MAX_ACTIVE_ENTRIES_PER_SECTOR`.
- Fleet `[fleet] kill_switch = true` blocks new entries for every account runtime that reads the
  registry.
- Fleet `[fleet] max_active_entries`, `max_active_entries_per_underlying`, and
  `max_active_entries_per_sector` cap exposure across enabled accounts using the account
  `state_path` values in the registry.
- Fleet account `risk_budget.max_buying_power_pct`, when configured, tightens the naked scanner's
  `max_buying_power_usage_pct` for that account.

Paper/live endpoint selection is controlled by `ALPACA_TRADING_BASE_URL` and
`ALPACA_TRADE_UPDATES_WS_URL`. Keep paper URLs in place until the rollout plan explicitly moves to a
tiny live canary.

## Operator Status

`alpaca-control operator` runs the `nautilus adapters alpaca status` binary and summarizes service state,
account status, open orders, positions, strategy state, the last structured scan/decision event, the
latest broker event, and operator alerts. Use `--json` for machine-readable output.

Common operator-status commands:

```bash
alpaca-control --account paper-main status
alpaca-control --account paper-main operator --json
alpaca-control --account paper-main health
alpaca-control today
```

`alpaca-control fleet` runs `nautilus adapters alpaca fleet`, reads the fleet registry, and executes per-account
operator status with the shared repo `.env` or an account's explicit env-file override. It is a
read-only fleet summary; it does not start services, submit orders, or change account state.

Common fleet commands:

```bash
alpaca-control accounts
alpaca-control fleet
alpaca-control fleet --json
alpaca-control fleet --include-disabled
nautilus adapters alpaca fleet --json
```

`alpaca-control validate` runs the targeted Alpaca formatting, shell syntax, library test, and binary
check commands used before installing runtime changes. `alpaca-control deploy` builds and installs
the local runtime without restarting services. `alpaca-control rollout` runs validation, deploys,
restarts every configured account service, verifies service activity, and prints the compact
`today` summary.

`alpaca-control today` prints compact fleet health. Candidate-ledger summaries are read from
Postgres by the performance report and alert commands.

Operator status reports fleet policy blocks as critical `fleet_policy_block` alerts. It also reports
partial fills and accepted/new orders that have not filled yet so stale order handling can be
separated from ordinary working-order latency. `last_management_snapshot` includes the latest
managed spread mark, net premium kind, unrealized PnL, hold time, days to expiration, and any active
close trigger.

Scanner diagnostics include a `rejections` map keyed by filter reason, so a no-candidate scan can be
attributed to liquidity, quote quality, delta range, POP/touch gates, buying-power usage, score, or
spread-construction filters instead of only reporting a generic no-candidate result.

Candidate alert commands read typed `candidate_alert` records from Postgres. They do not run
scanners or submit orders:

```bash
alpaca-control alerts candidates --all --dry-run
alpaca-control alerts candidates --all --send
alpaca-control alerts candidates --all --date 2026-05-08 --lookback-minutes 60 --max-rank 3
alpaca-control alerts enable
alpaca-control alerts status
alpaca-control alerts disable
```

Performance report commands read strategy state, Postgres candidate ledgers, broker activities, and current
positions. They do not submit or cancel orders:

```bash
alpaca-control performance --all
alpaca-control performance --all --json
alpaca-control performance --all --no-track-candidate-outcomes
alpaca-control performance --all --since 2026-05-01 --until 2026-05-08
alpaca-control alerts performance
alpaca-control alerts performance-enable
alpaca-control alerts performance-status
alpaca-control alerts performance-disable
```

When Postgres storage is configured, performance commands automatically append missing closed-entry
performance records and track candidate outcomes. Candidate outcomes use current option snapshots
first. When snapshots cannot value a candidate and the candidate has a timestamp, the performance
path can fill the mark from Alpaca historical option bars. Historical fills are enabled by default
and can be disabled or tuned with:

```bash
alpaca-control performance --no-track-historical-fill
alpaca-control performance --track-historical-fill-lookahead-minutes 390
alpaca-control performance --track-historical-fill-timeframe 1Min
```

Use `alpaca-control performance --all --no-track-candidate-outcomes` to read the existing
candidate-outcome analytics without adding new outcome rows.

The command reports one engine state:

- `idle`: service/account are healthy with no open broker exposure.
- `trading`: open orders or positions exist and no critical alert is active.
- `blocked`: the engine is intentionally blocked by kill-switch or disabled submission.
- `broken`: account, service, state, or exposure checks need operator intervention.

## Historical Opportunity and PnL Accounting

The Postgres candidate ledger is the source of historical opportunity evidence. Each scanner pass can append
ranked `candidate` records plus a `scanner_result`; selected or high-score candidates can append
`candidate_alert` records, and submission decisions append `decision` and `submit_result` records.
`alpaca-control performance --all --json` audits those records by account and trade date.

`alpaca-control performance` replays candidate-ledger records and values the same option symbols
from current snapshots or, when needed, Alpaca historical option bars. It writes Postgres
`candidate_outcome` records for observation buckets such as `plus_1h`, `same_day_close`,
`next_day`, and `expiration_risk`. Each record includes `mark_source`, currently `snapshot` or
`historical_bar`, so reports can separate current bid/ask-derived marks from historical bar-close
marks.

Candidate outcomes are analytical opportunity tracking; they are not broker fills and should not be
counted as realized trading PnL. Raw `candidate_outcome` record counts are observation counts, not
unique candidate counts: one candidate can appear in multiple buckets. Use the candidate outcome
analytics contract before tuning strategy thresholds from these records:
[Alpaca Candidate Outcome Analytics](alpaca_candidate_outcome_analytics.md).

Realized close PnL comes from broker activity reconstruction. The performance report matches each
strategy-state entry to opening and closing parent/leg order IDs, reads option activity cashflows,
and reports `realized_pnl = open_cashflow + close_cashflow` for closed entries when both sides are
available. Credit entries normally open with positive cashflow and close with negative cashflow;
debit entries normally open with negative cashflow and close with positive cashflow. Missing close
order IDs or fill activity produces warnings such as `missing_close_fills` or
`missing_realized_pnl` instead of fabricated PnL.

The engine appends realized close records during close handling when enough broker evidence is
available. The operator can also run `alpaca-control performance` to append missing closed entries.
The performance ledger is deduped by record key, so repeated backfills should not create duplicate
realized-trade records.

## Operator Runbook

Normal overnight monitoring:

```bash
nautilus adapters alpaca status --json
nautilus adapters alpaca fleet --json
cargo run -p nautilus-cli --features alpaca --bin nautilus -- adapters alpaca account
```

Expected overnight state with an open managed spread is service `active`, open orders `0`, unmanaged
positions `0`, and `last_decision.reason = outside_entry_window`.

Force flatten:

```bash
sed -i 's/^ALPACA_FORCE_FLATTEN=.*/ALPACA_FORCE_FLATTEN=true/' \
  .env
systemctl --user restart alpaca-options.service
nautilus adapters alpaca status --json
```

After the account is flat, set `ALPACA_FORCE_FLATTEN=false` and restart the service.

Stuck close order:

- Check `orders.open`, `active_entries[].close_attempts`, `last_management_snapshot`, and
  `last_management_block`.
- The live Nautilus strategy cancels stale close orders after `management.stale_close_secs`.
- If `close_attempts_exhausted` appears, inspect the spread, then either increase
  `management.max_close_attempts`, set `ALPACA_FORCE_FLATTEN=true`, or flatten manually at the
  broker.

Unmanaged position:

- Treat `unmanaged_positions` as critical.
- Do not enable more entries.
- Compare `nautilus adapters alpaca account` against the strategy state file, then either restore state
  from a known-good copy or flatten the unmanaged broker exposure.

Rejected MLeg:

- Check `recent_rejected_orders` and the latest broker event.
- Keep `max_active_entries = 1` and `max_open_orders = 1` until the rejection reason is understood.

## Log Rotation

For system logrotate, copy the example policy and adjust the username/path if needed:

```bash
sudo cp deploy/alpaca/alpaca-options.logrotate /etc/logrotate.d/alpaca-options
```

The service also writes stdout/stderr to journald through systemd, so `journalctl --user -u
alpaca-options.service` remains available.
