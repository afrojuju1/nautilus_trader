# Alpaca NUC Deployment

This deployment runs the Rust Alpaca options runner as one supervised user service for the
active account. It keeps secrets outside the repo, writes logs under the user's state directory, and
uses a file lock so only one runner can own a given Alpaca account workflow.

## Files

- `deploy/alpaca/alpaca-options-engine.env.example`: credentials, endpoints, service paths, and
  emergency override template.
- `deploy/alpaca/alpaca-options-engine.account.env.example`: account-scoped env template for future
  supervised account instances.
- `deploy/alpaca/alpaca-options-engine.base.toml.example`: shared strategy universe, scanner, sector,
  and management defaults inherited by account configs.
- `deploy/alpaca/alpaca-options-engine.toml.example`: strategy/scanner/management config template.
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

Edit `~/.config/nautilus-trader/alpaca/options-engine.env` and add Alpaca paper credentials. Keep
`ALPACA_KILL_SWITCH=true`, `ALPACA_SUBMIT=false`, `ALPACA_MANAGE=false`, and `ALPACA_CLOSE=false`
until paper proof is intentionally enabled.

The installed engine, operator status command, and account/order probe auto-load
`~/.config/nautilus-trader/alpaca/options-engine.env` when present. Set
`NAUTILUS_ALPACA_ENV_FILE` only when intentionally pointing at a different env file.

Edit `~/.config/nautilus-trader/alpaca/base-options-engine.toml` for shared scanner, universe,
sector, and management settings. Account configs can set top-level `extends` to inherit from that
base, then override only strategy identity, state path, risk caps, or account-specific scanner
limits. TOML `runtime.max_iterations = 0` is continuous service mode. Set
`ALPACA_MAX_ITERATIONS=1` only for a manual one-shot smoke test override.

Candidate-ledger evidence is enabled by default with `runtime.candidate_ledger_enabled = true`.
When `runtime.candidate_ledger_dir` is omitted, each account writes JSONL records under
`~/.local/state/nautilus_trader/alpaca/<account-id>/candidate-ledger/<trade-date>.jsonl`.
`runtime.candidate_ledger_max_candidates` controls how many ranked candidates per scanner result
are persisted; `0` records all ranked candidates.

Candidate Discord alerts are emitted by a sidecar command which consumes typed `candidate_alert`
records from the candidate ledger, not by the trading loop. Put the webhook in
`~/.config/nautilus-trader/alpaca/alerts.env` as `DISCORD_WEBHOOK_URL=...`; keep the file mode at
`600`.

The installer also creates `~/.config/nautilus-trader/alpaca/fleet.toml` if missing. The fleet
registry is read-only operator metadata; credentials remain in each account env file.

## Commands

```bash
alpaca-control accounts
alpaca-control today
alpaca-control ledger-summary --all
alpaca-control --account paper-main status
alpaca-control --account paper-directional status
alpaca-control --account paper-undefined-risk status
alpaca-control --account paper-undefined-risk check-config
alpaca-control --account paper-undefined-risk scan naked GDX,SLV
alpaca-control --account paper-directional scan directional XLF,XLK
alpaca-control --account paper-main ledger --lines 20
alpaca-control alerts candidates --all --dry-run
alpaca-control alerts candidates --all --send
alpaca-control alerts enable
alpaca-control alerts status
alpaca-control validate
alpaca-control deploy
alpaca-control rollout
alpaca-control fleet --json
alpaca-control health
alpaca-control logs
alpaca-fleet-status --json
```

`scan` is a one-shot diagnostic candidate scan. It auto-loads the selected account env/config,
forces dry-run entry decisions, disables submit/manage/close, ignores the entry window by default,
and raises local account caps so daily submit limits do not hide candidates. Use `run-once` when the
goal is to execute one normal engine iteration with the account's configured runtime gates.

Paper smoke tests that submit orders must be deliberate and short-lived. Keep paper endpoints in
the env file, set `ALPACA_MAX_ITERATIONS=1`, keep quantity at `1`, and set
`ALPACA_CANCEL_AFTER_ACCEPT=true` only for the smoke run. After the run, check account state with
`alpaca-control --account <account> status`; if any accepted smoke order remains open, cancel it in
the Alpaca paper dashboard or API before continuing. Do not leave accepted smoke orders working
unless the test explicitly requires it.

To enable start on user login:

```bash
systemctl --user enable alpaca-options.service
loginctl enable-linger "$USER"
```

## Runtime State

Default state files from the env template:

- Logs: `~/.local/state/nautilus_trader/logs/alpaca-options.log`
- Lock: `~/.local/state/nautilus_trader/locks/alpaca-options.lock`
- Strategy state: `~/.local/state/nautilus_trader/alpaca_options_engine_state.json`
- Candidate ledger: `~/.local/state/nautilus_trader/alpaca/<account-id>/candidate-ledger/*.jsonl`

The runner wrapper takes an exclusive non-blocking lock. If another process already owns the lock,
the service exits without starting another Alpaca account owner.

The service runs installed release binaries by default:

- Runner: `~/.local/bin/alpaca-options-engine`
- Operator status: `~/.local/bin/alpaca-operator-status`
- Fleet status: `~/.local/bin/alpaca-fleet-status`

Override with `NAUTILUS_ALPACA_RUNNER_BIN` or `NAUTILUS_ALPACA_OPERATOR_BIN` only for diagnostics.

Account-scoped services use template-owned log and lock directories, for example
`~/.local/state/nautilus_trader/alpaca/<account-id>/logs` and
`~/.local/state/nautilus_trader/alpaca/<account-id>/locks`. Keep each account's strategy state path
inside its account config so one account cannot read or write another account's runtime state.

## Multi-Account Foundation

The existing paper account remains `alpaca-options.service` and continues to use:

- Env: `~/.config/nautilus-trader/alpaca/options-engine.env`
- Config: `~/.config/nautilus-trader/alpaca/options-engine.toml`

Additional accounts are represented in `~/.config/nautilus-trader/alpaca/fleet.toml` with explicit
roles, permissions, risk budgets, service names, env files, config files, state files, log
directories, and lock directories. The account engine reads this registry on startup to enforce
account permissions, the fleet kill switch, and fleet-level active-entry caps before submitting new
entries.

For account-instance services, the systemd template owns account identity, service name, config
path, log path, and lock path. The account env file should stay focused on credentials, endpoints,
and explicit safety gates. Fleet status creates a hermetic operator-status child process for each
account instead of inheriting any `ALPACA_*` or `NAUTILUS_ALPACA_*` values from the parent shell.

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
`submit_enabled=false` and `kill_switch=true` for that runtime while leaving management/close gates
available for existing tracked exposure.

Future account env files should live under:

```text
~/.config/nautilus-trader/alpaca/accounts/<account-id>.env
```

Future account configs should live under:

```text
~/.config/nautilus-trader/alpaca/configs/<account-id>-options-engine.toml
```

Keep extra accounts disabled in the fleet registry and keep their env gates inert until credentials,
permissions, strategy config, and risk budget are reviewed:

```bash
ALPACA_SUBMIT=false
ALPACA_MANAGE=false
ALPACA_CLOSE=false
ALPACA_KILL_SWITCH=true
```

After an account env/config pair is reviewed, the account can be enabled explicitly:

```bash
systemctl --user start alpaca-options@paper-directional.service
```

Do not enable account-instance services on login until paper proof is complete for that account's
role and strategy set.

## Safety Gates

- `ALPACA_KILL_SWITCH=true` blocks new entries.
- `ALPACA_SUBMIT=true` allows entry submission.
- `ALPACA_MANAGE=true` allows stale-entry cancellation and management actions.
- `ALPACA_CLOSE=true` allows close order submission when management is enabled.
- `ALPACA_FORCE_FLATTEN=true` treats every tracked open spread as a close candidate.
- TOML `runtime.dry_run_strategies = ["iron_condor"]` lets a strategy scan and emit decisions
  without submitting while other enabled strategies can remain live.
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

`alpaca-control operator` runs the `alpaca-operator-status` binary and summarizes service state,
account status, open orders, positions, strategy state, the last structured scan/decision event, the
latest broker event, and operator alerts. Use `--json` for machine-readable output.

`alpaca-control fleet` runs `alpaca-fleet-status`, reads the fleet registry, and executes
per-account operator status with each account's env file. It is a read-only fleet summary; it does
not start services, submit orders, or change account state.

`alpaca-control validate` runs the targeted Alpaca formatting, shell syntax, library test, and binary
check commands used before installing runtime changes. `alpaca-control deploy` builds and installs
the local runtime without restarting services. `alpaca-control rollout` runs validation, deploys,
restarts every configured account service, verifies service activity, and prints the compact
`today` summary.

`alpaca-control today` prints compact fleet health plus per-account candidate-ledger counts.
`alpaca-control ledger-summary` prints record-type counts, latest submit results, latest decisions,
and same-day re-entry blocks for one account; add `--all` for every known local account.

Operator status reports fleet policy blocks as critical `fleet_policy_block` alerts. It also reports
partial fills and accepted/new orders that have not filled yet so stale order handling can be
separated from ordinary working-order latency. `last_management_snapshot` includes the latest
managed spread mark, net premium kind, unrealized PnL, hold time, days to expiration, and any active
close trigger.

Scanner diagnostics include a `rejections` map keyed by filter reason, so a no-candidate scan can be
attributed to liquidity, quote quality, delta range, POP/touch gates, buying-power usage, score, or
spread-construction filters instead of only reporting a generic no-candidate result.

The command reports one engine state:

- `idle`: service/account are healthy with no open broker exposure.
- `trading`: open orders or positions exist and no critical alert is active.
- `blocked`: the engine is intentionally blocked by kill-switch or disabled submission.
- `broken`: account, service, state, or exposure checks need operator intervention.

## Operator Runbook

Normal overnight monitoring:

```bash
alpaca-operator-status --json
alpaca-fleet-status --json
cargo run -p nautilus-alpaca --features live --bin alpaca-check-account-orders
```

Expected overnight state with an open managed spread is service `active`, open orders `0`, unmanaged
positions `0`, and `last_decision.reason = outside_entry_window`.

Force flatten:

```bash
sed -i 's/^ALPACA_FORCE_FLATTEN=.*/ALPACA_FORCE_FLATTEN=true/' \
  ~/.config/nautilus-trader/alpaca/options-engine.env
systemctl --user restart alpaca-options.service
alpaca-operator-status --json
```

After the account is flat, set `ALPACA_FORCE_FLATTEN=false` and restart the service.

Stuck close order:

- Check `orders.open`, `active_entries[].close_attempts`, `last_management_snapshot`, and
  `last_management_block`.
- The engine cancels stale close orders after `management.stale_close_secs`.
- If `close_attempts_exhausted` appears, inspect the spread, then either increase
  `management.max_close_attempts`, set `ALPACA_FORCE_FLATTEN=true`, or flatten manually at the
  broker.

Unmanaged position:

- Treat `unmanaged_positions` as critical.
- Do not enable more entries.
- Compare `alpaca-check-account-orders` against the strategy state file, then either restore state
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
