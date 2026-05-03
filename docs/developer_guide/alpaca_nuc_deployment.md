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

Edit `~/.config/nautilus-trader/alpaca/index-credit.toml` for strategy/scanner/management settings.
TOML `runtime.max_iterations = 0` is continuous service mode. Set `ALPACA_MAX_ITERATIONS=1` only
for a manual one-shot smoke test override.

## Commands

```bash
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

## Log Rotation

For system logrotate, copy the example policy and adjust the username/path if needed:

```bash
sudo cp deploy/alpaca/alpaca-index-credit.logrotate /etc/logrotate.d/alpaca-index-credit
```

The service also writes stdout/stderr to journald through systemd, so `journalctl --user -u
alpaca-index-credit.service` remains available.
