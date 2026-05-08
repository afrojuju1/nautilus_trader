# Repo Instructions

## Core

- This fork is the target trading engine for the migration. Build new live trading runtime work in Nautilus-native code rather than adding new `spreads` runtime dependencies.
- Keep changes focused and in the style of the upstream Nautilus Trader codebase.
- Do not commit or push unless the user explicitly asks.
- Do not rewrite published fork history unless the user explicitly asks for a force-push workflow.
- Prefer non-destructive Git operations. Do not use `git reset --hard`, `git checkout --`, or branch deletion as part of normal sync work.
- Do not add the long Nautech copyright/license banner block to new or edited files unless the user explicitly asks for it.

## Upstream Sync

When syncing this fork with `nautechsystems/nautilus_trader`, preserve fork-only commits by merging upstream into the fork.

Recommended workflow from the repo root:

```bash
git status --short
git fetch origin
git fetch upstream
git checkout develop
git pull --ff-only origin develop
git branch backup/develop-before-upstream-sync-$(date +%Y%m%d-%H%M%S)
git merge --no-ff upstream/develop -m "Merge upstream develop"
```

After the merge:

```bash
git log --oneline upstream/develop..develop
cargo check -p nautilus-alpaca --features live --bins
git push origin develop
```

Rules:

- Use merge for normal syncs. Do not reset `develop` to `upstream/develop`; that discards fork-only commits.
- Check `git log --oneline upstream/develop..develop` before pushing so fork-only commits are still visible.
- If conflicts occur, resolve them in favor of preserving our Alpaca adapter/runtime work unless the user explicitly decides otherwise.
- Keep the backup branch until the pushed fork has been verified.

## Alpaca Adapter Work

- For Alpaca execution changes, run targeted checks before commit:

```bash
cargo fmt -p nautilus-alpaca
cargo test -p nautilus-alpaca --features live --lib
cargo check -p nautilus-alpaca --features live --bins
```

- Keep Alpaca verification tiered:
- Unit/config/strategy/order changes: run the targeted checks above.
- Build-only or docs-only deploy changes: run `cargo fmt -p nautilus-alpaca` and `cargo check -p nautilus-alpaca --features live --bins` when Rust code or scripts can affect binaries.
- Broker/account probes: run only when touching account admission, execution submission, order reconciliation, or before/after a smoke test.
- Systemd install/control checks: run only when deployment files, installed binaries, or service wiring changes.
- Paper order smoke tests: run only intentionally, preferably during market hours, and cancel accepted smoke orders unless the user explicitly asks to leave orders open.
- Live or paper Alpaca smoke tests must be real. Do not claim execution proof unless an actual command was run and the resulting account/orders state was checked.
- Smoke tests that submit paper orders should cancel accepted orders unless the user explicitly asks to leave orders open.
- Earnings-calendar input for Alpaca earnings strategies uses Alpha Vantage only through local secrets and cache. Keep `ALPHA_VANTAGE_API_KEY` in an untracked `.env` or external env file, never commit it. Refresh with `earnings-sync`; it caches raw Alpha Vantage `EARNINGS_CALENDAR` output for 23 hours by default, writes the full normalized feed to `$XDG_STATE_HOME/nautilus_trader/earnings/earnings_events.csv` or `$HOME/.local/state/nautilus_trader/earnings/earnings_events.csv`, and writes the stricter strategy-safe feed to `earnings_events_approved.csv` in the same directory.
- Treat Alpha Vantage earnings timing quality as mixed: `pre-market` and `post-market` can be normalized to `before_open` and `after_close`, but blank timing becomes `unknown` and must remain blocked by default unless the user explicitly approves unknown-timing entries. The approved feed should also exclude weekend dates and non-common symbol shapes before any strategy consumes it.
