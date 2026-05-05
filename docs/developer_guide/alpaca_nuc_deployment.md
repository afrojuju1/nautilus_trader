# Alpaca NUC Deployment

This deployment runs the Rust Alpaca index credit runner as one supervised user service for the
active account. It keeps secrets outside the repo, writes logs under the user's state directory, and
uses a file lock so only one runner can own a given Alpaca account workflow.

## Files

- `deploy/alpaca/alpaca-index-credit.env.example`: credentials, endpoints, service paths, and
  emergency override template.
- `deploy/alpaca/alpaca-index-credit.account.env.example`: account-scoped env template for future
  supervised account instances.
- `deploy/alpaca/alpaca-index-credit.toml.example`: strategy/scanner/management config template.
- `deploy/alpaca/alpaca-fleet.toml.example`: read-only fleet registry template.
- `deploy/alpaca/alpaca-index-credit-install.sh`: builds release binaries and installs user files.
- `deploy/alpaca/alpaca-index-credit-runner.sh`: `flock`-guarded runner wrapper.
- `deploy/alpaca/alpaca-index-credit.service`: user systemd service.
- `deploy/alpaca/alpaca-index-credit@.service`: disabled-by-default account-instance service
  template.
- `deploy/alpaca/alpaca-index-credit-control.sh`: operator status, health, start, stop, restart,
  fleet status, logs.
- `deploy/alpaca/alpaca-index-credit.logrotate`: optional logrotate policy.

## Install

From the repo root:

```bash
deploy/alpaca/alpaca-index-credit-install.sh
```

Edit `~/.config/nautilus-trader/alpaca/index-credit.env` and add Alpaca paper credentials. Keep
`ALPACA_KILL_SWITCH=true`, `ALPACA_SUBMIT=false`, `ALPACA_MANAGE=false`, and `ALPACA_CLOSE=false`
until paper proof is intentionally enabled.

The installed engine, operator status command, and account/order probe auto-load
`~/.config/nautilus-trader/alpaca/index-credit.env` when present. Set
`NAUTILUS_ALPACA_ENV_FILE` only when intentionally pointing at a different env file.

Edit `~/.config/nautilus-trader/alpaca/index-credit.toml` for strategy/scanner/management settings.
TOML `runtime.max_iterations = 0` is continuous service mode. Set `ALPACA_MAX_ITERATIONS=1` only
for a manual one-shot smoke test override.

The installer also creates `~/.config/nautilus-trader/alpaca/fleet.toml` if missing. The fleet
registry is read-only operator metadata; credentials remain in each account env file.

## Commands

```bash
alpaca-index-credit-engine --check-config
systemctl --user start alpaca-index-credit.service
systemctl --user stop alpaca-index-credit.service
systemctl --user status alpaca-index-credit.service --no-pager
deploy/alpaca/alpaca-index-credit-control.sh operator
deploy/alpaca/alpaca-index-credit-control.sh operator --json
deploy/alpaca/alpaca-index-credit-control.sh fleet
deploy/alpaca/alpaca-index-credit-control.sh fleet --json
deploy/alpaca/alpaca-index-credit-control.sh health
deploy/alpaca/alpaca-index-credit-control.sh logs
alpaca-fleet-status --json
```

To enable start on user login:

```bash
systemctl --user enable alpaca-index-credit.service
loginctl enable-linger "$USER"
```

## Runtime State

Default state files from the env template:

- Logs: `~/.local/state/nautilus_trader/logs/alpaca-index-credit.log`
- Lock: `~/.local/state/nautilus_trader/locks/alpaca-index-credit.lock`
- Strategy state: `~/.local/state/nautilus_trader/alpaca_index_credit_state.json`

The runner wrapper takes an exclusive non-blocking lock. If another process already owns the lock,
the service exits without starting another Alpaca account owner.

The service runs installed release binaries by default:

- Runner: `~/.local/bin/alpaca-index-credit-engine`
- Operator status: `~/.local/bin/alpaca-operator-status`
- Fleet status: `~/.local/bin/alpaca-fleet-status`

Override with `NAUTILUS_ALPACA_RUNNER_BIN` or `NAUTILUS_ALPACA_OPERATOR_BIN` only for diagnostics.

Account-scoped services use template-owned log and lock directories, for example
`~/.local/state/nautilus_trader/alpaca/<account-id>/logs` and
`~/.local/state/nautilus_trader/alpaca/<account-id>/locks`. Keep each account's strategy state path
inside its account config so one account cannot read or write another account's runtime state.

## Multi-Account Foundation

The existing paper account remains `alpaca-index-credit.service` and continues to use:

- Env: `~/.config/nautilus-trader/alpaca/index-credit.env`
- Config: `~/.config/nautilus-trader/alpaca/index-credit.toml`

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

- `put`, `put_credit`, `index_put_credit_entry`: put-credit verticals.
- `call`, `call_credit`, `index_call_credit_entry`: call-credit verticals.
- `iron_condor`, `condor`, `index_iron_condor_entry`: four-leg iron condors.
- `call_debit`, `index_call_debit_entry`: long call-debit verticals.
- `put_debit`, `index_put_debit_entry`: long put-debit verticals.
- `debit`, `long_premium`, `directional`: both call-debit and put-debit verticals.

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
~/.config/nautilus-trader/alpaca/configs/<account-id>-index-credit.toml
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
systemctl --user start alpaca-index-credit@paper-directional.service
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
- TOML `[risk] max_active_entries`, `max_daily_submits`, and `max_open_orders` cap account-level
  exposure before any hosted strategy can submit. Env overrides are available as
  `ALPACA_MAX_ACTIVE_ENTRIES`, `ALPACA_MAX_DAILY_SUBMITS`, and `ALPACA_MAX_OPEN_ORDERS`.
- TOML `[risk] max_active_entries_per_underlying`, `max_active_entries_per_sector`, and
  `[risk.sectors]` cap concentration by symbol and configured correlation group. Env overrides are
  available for the two numeric caps as `ALPACA_MAX_ACTIVE_ENTRIES_PER_UNDERLYING` and
  `ALPACA_MAX_ACTIVE_ENTRIES_PER_SECTOR`.
- Fleet `[fleet] kill_switch = true` blocks new entries for every account runtime that reads the
  registry.
- Fleet `[fleet] max_active_entries`, `max_active_entries_per_underlying`, and
  `max_active_entries_per_sector` cap exposure across enabled accounts using the account
  `state_path` values in the registry.

Paper/live endpoint selection is controlled by `ALPACA_TRADING_BASE_URL` and
`ALPACA_TRADE_UPDATES_WS_URL`. Keep paper URLs in place until the rollout plan explicitly moves to a
tiny live canary.

## Operator Status

`alpaca-index-credit-control.sh operator` runs the `alpaca-operator-status` binary and summarizes
service state, account status, open orders, positions, strategy state, the last structured
scan/decision event, the latest broker event, and operator alerts. Use `--json` for machine-readable
output.

`alpaca-index-credit-control.sh fleet` runs `alpaca-fleet-status`, reads the fleet registry, and
executes per-account operator status with each account's env file. It is a read-only fleet summary;
it does not start services, submit orders, or change account state.

Operator status reports fleet policy blocks as critical `fleet_policy_block` alerts. It also reports
partial fills and accepted/new orders that have not filled yet so stale order handling can be
separated from ordinary working-order latency. `last_management_snapshot` includes the latest
managed spread mark, net premium kind, unrealized PnL, hold time, days to expiration, and any active
close trigger.

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
  ~/.config/nautilus-trader/alpaca/index-credit.env
systemctl --user restart alpaca-index-credit.service
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
sudo cp deploy/alpaca/alpaca-index-credit.logrotate /etc/logrotate.d/alpaca-index-credit
```

The service also writes stdout/stderr to journald through systemd, so `journalctl --user -u
alpaca-index-credit.service` remains available.
