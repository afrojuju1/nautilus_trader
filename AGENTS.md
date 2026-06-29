# Repo Instructions

## Core

- This fork is the target trading engine for the migration. Build new live trading runtime work in Nautilus-native code rather than adding new `spreads` runtime dependencies.
- Keep changes focused and in the style of the upstream Nautilus Trader codebase.
- Do not commit or push unless the user explicitly asks.
- Do not rewrite published fork history unless the user explicitly asks for a force-push workflow.
- Prefer non-destructive Git operations. Do not use `git reset --hard`, `git checkout --`, or branch deletion as part of normal sync work.
- Do not add the long Nautech copyright/license banner block to new or edited files unless the user explicitly asks for it.

## Upstream Sync

When syncing this fork with the base repo `nautechsystems/nautilus_trader`, preserve fork-only
commits by merging upstream into the fork. Use the repo-local `sync-nautilus-fork` Codex skill at
`.codex/skills/sync-nautilus-fork` for the full workflow when available.

Do this only when the user asks to proceed with a sync. For status, health, review, or "how far out
of sync" requests, stay read-only and report the current branch, remotes, ahead/behind counts, likely
conflict areas, and recommended next step.

Recommended workflow from the repo root:

```bash
git status --short
git remote -v
git fetch origin
git fetch upstream
git pull --ff-only origin develop
git branch backup/develop-before-upstream-sync-$(date +%Y%m%d-%H%M%S)
git merge --no-ff upstream/develop -m "Merge upstream develop"
```

If `upstream` is missing, add it as `https://github.com/nautechsystems/nautilus_trader.git` only as
part of an explicit sync workflow.

After the merge:

```bash
git log --oneline upstream/develop..develop
cargo check -p nautilus-alpaca --features live --bins
git push origin develop
```

Rules:

- Use merge for normal syncs. Do not reset `develop` to `upstream/develop`; that discards fork-only commits.
- Do not rebase published fork history unless the user explicitly asks for a force-push workflow.
- Check `git log --oneline upstream/develop..develop` before pushing so fork-only commits are still visible.
- If conflicts occur, resolve them in favor of preserving our Alpaca adapter/runtime work unless the user explicitly decides otherwise.
- Read both sides of conflicted files before editing. Prefer upstream structure plus fork-owned Alpaca behavior when both can coexist.
- After resolving conflicts, run `git diff --check` and the relevant build/check commands before committing or pushing.
- Keep the backup branch until the pushed fork has been verified.
- Do not push the merge unless the user explicitly asks to push.

## Alpaca Adapter Work

- The order-capable Alpaca runtime is `alpaca-options-node`, wired through
  Nautilus-native actor/strategy/data/execution paths. Use `alpaca-options-node --check-config`
  for config checks.
- Use the repo-local `.env` as the canonical Alpaca operator env file for local, Docker, and
  single-host deployed workflows. `NAUTILUS_ALPACA_ENV_FILE` is only for deliberate account or
  diagnostic overrides; do not make config-home `options.env` the default path.
- Do not restore retired handoff or standalone strategy-loop surfaces such as
  `alpaca-submit-order-list-bridge`, `submit_order_list_bridge.rs`, or
  `alpaca-put-credit-strategy-loop` unless Ade explicitly asks for a staged bridge.
- Use the normal runtime submission gate `ALPACA_SUBMIT`; do not reintroduce
  `ALPACA_OPTIONS_LIVE_ENTRY_SUBMIT_ENABLED`.
- Live submit readiness must keep the Alpaca account capability preflight active. `alpaca-options-node`
  checks account options approval/trading level, account configuration max options level, buying
  power, and account configuration trading blocks before starting broker submission.
- Assignment, exercise, and expiration lifecycle risk belongs in the `alpaca-options-node` /
  `AlpacaOptionsStrategy` path. Use the account-activity lifecycle poller and strategy lifecycle
  admission block; do not add a standalone lifecycle loop unless Ade explicitly asks for a separate
  operator daemon.
- Active-risk option quote freshness belongs in `AlpacaOptionsStrategy` through Nautilus
  `subscribe_quotes` and the strategy cache. Do not add a standalone quote loop; improve the Alpaca
  data client when quote transport needs to move from REST snapshots to streaming.
- Historical candidate replay belongs in the read-only `alpaca-ops replay` / `performance` research
  path. It may read candidate ledgers and historical market data, but must not submit orders, cancel
  orders, reconcile broker state, or mutate strategy state.
- Do not wire or stamp regime routing metadata until real feature inputs exist. Avoid placeholder
  neutral regime labels because they make candidate evidence look more complete than it is.
- For Alpaca execution changes, run targeted checks before commit:

```bash
cargo fmt -p nautilus-alpaca
cargo test -p nautilus-alpaca --features live --lib
cargo check -p nautilus-alpaca --features live --bins
```

Keep Alpaca verification tiered:

- Unit/config/strategy/order changes: run the targeted checks above.
- Build-only or docs-only deploy changes: run `cargo fmt -p nautilus-alpaca` and `cargo check -p nautilus-alpaca --features live --bins` when Rust code or scripts can affect binaries.
- Broker/account probes: run only when touching account admission, execution submission, order reconciliation, or before/after a smoke test.
- Systemd install/control checks: run only when deployment files, installed binaries, or service wiring changes.
- Paper order smoke tests: run only intentionally, preferably during market hours, and cancel accepted smoke orders unless the user explicitly asks to leave orders open.
- Live or paper Alpaca smoke tests must be real. Do not claim execution proof unless an actual command was run and the resulting account/orders state was checked.
- Smoke tests that submit paper orders should cancel accepted orders unless the user explicitly asks to leave orders open.
- For Docker Alpaca rollouts, rebuild/recreate `alpaca-options` with the repo-local `.env` and
  container-readable config mounts, then verify the running stack with:

```bash
docker compose --env-file .env -f deploy/alpaca/compose.yml --profile engine ps
docker exec nautilus-alpaca-alpaca-options-1 alpaca-ops status --json
docker exec nautilus-alpaca-alpaca-options-1 alpaca-options-node --check-config
```

- Do not claim live Alpaca proof unless a real broker/account/status/order check was run and the
  result is reported. Compile checks and container health are build/runtime proof only.
- Earnings-calendar input for Alpaca earnings strategies uses Alpha Vantage only through local secrets and cache. Keep `ALPHA_VANTAGE_API_KEY` in an untracked `.env` or external env file, never commit it. Refresh with `earnings-sync`; it caches raw Alpha Vantage `EARNINGS_CALENDAR` output for 23 hours by default, writes the full normalized feed to `$XDG_STATE_HOME/nautilus_trader/earnings/earnings_events.csv` or `$HOME/.local/state/nautilus_trader/earnings/earnings_events.csv`, and writes the stricter strategy-safe feed to `earnings_events_approved.csv` in the same directory.
- Treat Alpha Vantage earnings timing quality as mixed: `pre-market` and `post-market` can be normalized to `before_open` and `after_close`, but blank timing becomes `unknown` and must remain blocked by default unless the user explicitly approves unknown-timing entries. The approved feed should also exclude weekend dates and non-common symbol shapes before any strategy consumes it.

## Storage And Warehouse Architecture

- Use the repo-local `market-data-warehouse` Codex skill at
  `.codex/skills/market-data-warehouse` for ClickHouse, catalog backfill, dual-write, flagged
  read-cutover, and Postgres operational-storage work when available.
- ClickHouse is a repo-level analytical market-data warehouse, not an Alpaca adapter feature. Alpaca
  data may be the first proof source, but warehouse schema, migrations, deployment, and read paths
  should stay source-neutral.
- Keep `ParquetDataCatalog` as the replay/backtest-compatible store. Write market data to both the
  catalog and ClickHouse during the initial rollout, then cut reads over to ClickHouse only by an
  explicit flag after validation.
- Keep Postgres as the slim operational source of truth for strategy state, state events, candidate
  ledgers, candidate outcomes, performance ledgers, runtime leases, and small ingest manifests.
- Do not store bulk quotes, trades, bars, Greeks, scanner feature series, or market-data-shaped
  caches in Postgres. Do not make ClickHouse the source of truth for broker evidence or realized
  PnL.
- Use `sqlx` migrations for Postgres operational schema changes. Do not add a custom Postgres
  migration manager.
- Avoid `shadow_compare`, standing comparison loops, or other one-off validation daemons. Add
  explicit operator validation commands for requested dataset/range checks and keep cutover
  rollback as a simple config flag.

## Issue Tracking

This repo uses `bd` (Beads) as the durable work ledger. Run `bd prime` for current workflow context
and command help, but repo and user instructions override any generic Beads session-close protocol.

- Use `bd ready` to find unblocked work.
- Use `bd create`, `bd update <id> --claim`, and `bd close <id>` for substantial implementation
  tasks and meaningful discoveries.
- Keep Beads in sync when committing Beads-backed work, but do not commit or push repository changes
  unless the user explicitly asks.
- Do not replace short-lived implementation checklists with Beads unless the work should persist
  beyond the current turn.
