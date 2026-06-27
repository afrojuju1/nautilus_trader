---
name: sync-nautilus-fork
description: Safely sync Ade's Nautilus Trader fork with the base upstream repository. Use when asked to check how far the fork is out of sync, pull in upstream/base changes, merge `nautechsystems/nautilus_trader` into the fork, resolve upstream sync conflicts, verify fork-only commits remain, or push a completed upstream sync.
---

# Sync Nautilus Fork

## Goal

Sync `/home/ade/Projects/nautilus_trader` with `nautechsystems/nautilus_trader` by merging
upstream into the fork, never by resetting the fork to upstream. Preserve fork-only Alpaca adapter,
runtime, docs, deployment, Beads, repo-level warehouse, ClickHouse, `nautilus-persistence`, and
sqlx/Postgres operational-storage work unless Ade explicitly decides otherwise. The current Alpaca
order-capable runtime is `alpaca-options-node`; run `alpaca-options-node --check-config` for config
checks.

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
old_upstream=$(git rev-parse upstream/develop 2>/dev/null || true)
git fetch origin
git fetch upstream
git log --oneline --decorate --graph --left-right --cherry-pick origin/develop...upstream/develop
```

When `old_upstream` is available, include a storage/schema drift scan before merging:

```bash
git diff --name-only "${old_upstream}..upstream/develop" -- '*migration*' 'schema/**' 'crates/persistence/**' 'nautilus_trader/persistence/**' 'nautilus_trader/adapters/**' 'crates/adapters/**' 'docs/**'
```

Review matches for new database columns, migrations, storage models, catalog/ClickHouse contracts,
Postgres schema changes, persistence config, or data-reader/writer semantics that fork-only Alpaca
or warehouse work may need to adopt.

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
- Preserving the Nautilus-native Alpaca runtime path over repo-local bridges, wrappers, or old
  account-engine loops.
- Preserving repo-level market-data warehouse ownership over adapter-owned ClickHouse modules,
  migrations, deployment files, or one-off comparison loops.

Do not resurrect retired Alpaca artifacts during conflict resolution unless Ade explicitly asks for a
new staged bridge:

- `alpaca-submit-order-list-bridge` / `submit_order_list_bridge.rs`
- `alpaca-put-credit-strategy-loop` / `put_credit_strategy_loop.rs`
- `ALPACA_OPTIONS_LIVE_ENTRY_SUBMIT_ENABLED`

Use `ALPACA_SUBMIT` as the runtime submit gate.

Do not turn ClickHouse or warehouse code into an Alpaca-only sync resolution. Warehouse work belongs
under repo-level ownership such as `nautilus-persistence`, `schema/sql/clickhouse/`, and
`deploy/warehouse/`.

After resolving conflicts:

```bash
git status --short
git diff --check
rg -n "alpaca-submit-order-list-bridge|submit_order_list_bridge|alpaca-put-credit-strategy-loop|put_credit_strategy_loop|ALPACA_OPTIONS_LIVE_ENTRY_SUBMIT_ENABLED" crates/adapters/alpaca deploy/alpaca nautilus_trader/adapters/alpaca
rg -n "shadow_compare|ALPACA_.*CLICKHOUSE|ClickHouse" crates/adapters/alpaca deploy/alpaca nautilus_trader/adapters/alpaca
git log --oneline upstream/develop..develop
```

Confirm the fork-only commits are still visible before committing or pushing.
Matches from the warehouse ownership scan require review because ClickHouse should stay
source-neutral and outside Alpaca-owned paths.

## Schema And Storage Parity

Treat upstream database, catalog, and persistence changes as first-class follow-up work, not as
chat-only notes.

During sync, inspect upstream changes to:

- SQL migrations, schema files, and generated database metadata.
- Postgres, ClickHouse, catalog, cache, and persistence crates/modules.
- Rust/Python storage models, row structs, serializers, data readers, and writer config.
- Operator commands or docs that describe new storage fields, migrations, or read/write behavior.

If a storage/schema change is required for the merge to compile or for the fork runtime to keep
working, adapt it in the sync and validate it before finishing.

If upstream added storage capability that the fork should support but it is not necessary to resolve
inside the sync, create a Bead before the final response. Use a concrete title and acceptance
criteria, for example:

```bash
bd create "Adapt Alpaca strategy_state to upstream <column-or-contract>" \
  --type task \
  --priority 1 \
  --label upstream-sync \
  --label storage-parity
```

Good parity Beads name the upstream change, the fork-owned surface that must adapt, and the proof
needed. Create these for items such as:

- New or renamed Postgres columns that fork-owned queries, migrations, or read models should carry.
- New persistence metadata required by upstream readers/writers.
- Catalog or ClickHouse table-contract changes that affect warehouse backfill, dual-write, or
  flagged read cutover.
- New storage config/env fields that deployment, Docker, or operator commands need to expose.
- Upstream abstractions that let fork-owned storage code be deleted or moved into a base path.

Do not create Beads for upstream storage changes with no plausible fork impact. If none are found,
say that in the final summary.

If a parity item is completed during the sync, close the Bead with validation notes. Otherwise leave
it open and include it under remaining risks/follow-ups in the final response.

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

## Upstream Improvement Summary

After a completed sync, include a short operator-facing summary of upstream improvements that the
fork can build on for Alpaca work. Keep it concise and grouped by practical relevance, not by every
commit.

Use the previous upstream ref captured during preflight when available:

```bash
git log --oneline "${old_upstream}..upstream/develop"
```

If `old_upstream` was unavailable, summarize from the fetched graph or from the merge commit range.

Classify improvements with Alpaca impact in mind:

- **Direct Alpaca impact**: mention only if upstream changed Alpaca-owned files or APIs used by the
  Alpaca adapter.
- **Useful foundation**: live/runtime, timers, strategy/order cache, execution, msgbus, data,
  persistence, ClickHouse/catalog, Python node/examples, build/CI/dependency/security changes that
  can improve or simplify Alpaca work.
- **Cleanup opportunities**: upstream abstractions that let us delete fork-local Alpaca code,
  wrapper paths, or duplicated runtime logic.
- **Background upstream changes**: adapters, docs, tests, or examples that landed but do not affect
  Alpaca directly.

Prefer this shape in the final response:

```text
Pushed. Worktree clean.

New upstream/base improvements now available to build on for Alpaca:
- Live/runtime: ...
- Strategy/execution: ...
- Data/persistence: ...
- Build/CI/deps: ...

Direct Alpaca changes: none found / <brief note>.
```

Do not overstate impact. Say "relevant foundation" when the change helps the platform Alpaca runs on,
and reserve "direct Alpaca" for changes that actually touch Alpaca code or contracts.

Report the merge commit, validation performed, push result, remaining risks, and the upstream
improvement summary.
