---
name: sync-nautilus-fork
description: Safely sync Ade's Nautilus Trader fork with the base upstream repository. Use when asked to check how far the fork is out of sync, pull in upstream/base changes, merge `nautechsystems/nautilus_trader` into the fork, resolve upstream sync conflicts, verify fork-only commits remain, or push a completed upstream sync.
---

# Sync Nautilus Fork

## Goal

Sync `/home/ade/Projects/nautilus_trader` with `nautechsystems/nautilus_trader` by merging
upstream into the fork, never by resetting the fork to upstream. Preserve fork-only Alpaca adapter,
runtime, docs, and deployment work unless Ade explicitly decides otherwise.

## Modes

- For "status", "how out of sync", "review", or "health" requests, stay read-only. Report branch,
  remotes, ahead/behind counts, important fork-only commits, likely conflict areas, and the exact
  next step.
- For "pull it in", "proceed", "sync it", or similar requests, run the merge workflow.
- Commit or push only when Ade explicitly asks. A merge commit is acceptable when Ade has explicitly
  asked to proceed with the sync.

## Preflight

From the repo root:

```bash
git status --short --branch
git remote -v
git fetch origin
git fetch upstream
git log --oneline --decorate --graph --left-right --cherry-pick origin/develop...upstream/develop
```

If `upstream` is missing during an explicit sync, add:

```bash
git remote add upstream https://github.com/nautechsystems/nautilus_trader.git
git fetch upstream
```

If the worktree has unrelated user changes, do not revert them. Either work around them or explain
the risk before merging.

## Merge Workflow

Use non-destructive git operations:

```bash
git pull --ff-only origin develop
git branch backup/develop-before-upstream-sync-$(date +%Y%m%d-%H%M%S)
git merge --no-ff upstream/develop -m "Merge upstream develop"
```

Do not use `git reset --hard`, `git checkout --`, rebase published fork history, or force-push
unless Ade explicitly asks for that workflow.

## Conflict Policy

Resolve conflicts by reading both sides first.

Prefer:

- Upstream structure, naming, and generated changes where they are base-project ownership.
- Fork-owned Alpaca adapter/runtime behavior where it is Ade's migration target.
- Removing stale compatibility paths when Ade has asked for cleanup or replacement.
- Keeping active docs and examples aligned with the current architecture.

After resolving conflicts:

```bash
git status --short
git diff --check
git log --oneline upstream/develop..develop
```

Confirm the fork-only commits are still visible before committing or pushing.

## Validation

Run repo-local checks that match the affected surface. For Alpaca execution/runtime changes, run:

```bash
cargo fmt -p nautilus-alpaca
cargo test -p nautilus-alpaca --features live --lib
cargo check -p nautilus-alpaca --features live --bins
```

For Python example/config changes, prefer `uv run --active --no-sync ...` and run targeted `ruff`
or config checks. If live/paper Alpaca validation is requested, use paper credentials, check account
state before and after, and cancel accepted smoke orders unless Ade explicitly asks to leave them
open.

## Finish

Before push:

```bash
git status --short --branch
git log --oneline upstream/develop..develop
```

Push only when Ade asks:

```bash
git push origin develop
```

Report the merge commit, validation performed, push result, and any remaining risks.
