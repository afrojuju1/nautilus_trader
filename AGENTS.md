# Repo Instructions

## Core

- This fork is the target trading engine for the migration. Build new live trading runtime work in Nautilus-native code rather than adding new `spreads` runtime dependencies.
- Keep changes focused and in the style of the upstream Nautilus Trader codebase.
- Do not commit or push unless the user explicitly asks.
- Do not rewrite published fork history unless the user explicitly asks for a force-push workflow.
- Prefer non-destructive Git operations. Do not use `git reset --hard`, `git checkout --`, or branch deletion as part of normal sync work.

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
cargo check -p nautilus-alpaca --features live --bins
```

- Live or paper Alpaca smoke tests must be real. Do not claim execution proof unless an actual command was run and the resulting account/orders state was checked.
- Smoke tests that submit paper orders should cancel accepted orders unless the user explicitly asks to leave orders open.
