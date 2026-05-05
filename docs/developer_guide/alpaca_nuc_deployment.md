# Alpaca NUC Deployment

This deployment runs the Rust Alpaca index credit runner as one supervised user service. It keeps
secrets outside the repo, writes logs under the user's state directory, and uses a file lock so only
one runner can own the Alpaca account workflow.

## Files

- `deploy/alpaca/alpaca-index-credit.env.example`: credentials, endpoints, service paths, and
  emergency override template.
- `deploy/alpaca/alpaca-index-credit.toml.example`: strategy/scanner/management config template.
- `deploy/alpaca/alpaca-index-credit-install.sh`: builds release binaries and installs user files.
- `deploy/alpaca/alpaca-index-credit-runner.sh`: `flock`-guarded runner wrapper.
- `deploy/alpaca/alpaca-index-credit.service`: user systemd service.
- `deploy/alpaca/alpaca-index-credit-control.sh`: operator status, health, start, stop, restart,
  logs.
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

## Commands

```bash
alpaca-index-credit-engine --check-config
systemctl --user start alpaca-index-credit.service
systemctl --user stop alpaca-index-credit.service
systemctl --user status alpaca-index-credit.service --no-pager
deploy/alpaca/alpaca-index-credit-control.sh operator
deploy/alpaca/alpaca-index-credit-control.sh operator --json
deploy/alpaca/alpaca-index-credit-control.sh health
deploy/alpaca/alpaca-index-credit-control.sh logs
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

Override with `NAUTILUS_ALPACA_RUNNER_BIN` or `NAUTILUS_ALPACA_OPERATOR_BIN` only for diagnostics.

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
- TOML `[risk] max_active_entries`, `max_daily_submits`, and `max_open_orders` cap account-level
  exposure before any hosted strategy can submit. Env overrides are available as
  `ALPACA_MAX_ACTIVE_ENTRIES`, `ALPACA_MAX_DAILY_SUBMITS`, and `ALPACA_MAX_OPEN_ORDERS`.
- TOML `[risk] max_active_entries_per_underlying`, `max_active_entries_per_sector`, and
  `[risk.sectors]` cap concentration by symbol and configured correlation group. Env overrides are
  available for the two numeric caps as `ALPACA_MAX_ACTIVE_ENTRIES_PER_UNDERLYING` and
  `ALPACA_MAX_ACTIVE_ENTRIES_PER_SECTOR`.

Paper/live endpoint selection is controlled by `ALPACA_TRADING_BASE_URL` and
`ALPACA_TRADE_UPDATES_WS_URL`. Keep paper URLs in place until the rollout plan explicitly moves to a
tiny live canary.

## Operator Status

`alpaca-index-credit-control.sh operator` runs the `alpaca-operator-status` binary from the repo and
summarizes service state, account status, open orders, positions, strategy state, the last structured
scan/decision event, the latest broker event, and operator alerts. Use `--json` for machine-readable
output.

The command reports one engine state:

- `idle`: service/account are healthy with no open broker exposure.
- `trading`: open orders or positions exist and no critical alert is active.
- `blocked`: the engine is intentionally blocked by kill-switch or disabled submission.
- `broken`: account, service, state, or exposure checks need operator intervention.

## Operator Runbook

Normal overnight monitoring:

```bash
alpaca-operator-status --json
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

- Check `orders.open`, `active_entries[].close_attempts`, and `last_management_block`.
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
