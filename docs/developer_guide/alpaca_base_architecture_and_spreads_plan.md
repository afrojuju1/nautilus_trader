# Nautilus Base, Alpaca Runtime, And Spreads Migration Plan

This document describes the current architecture of the Nautilus base system and
the Alpaca options runtime, then compares it with the `spreads` project. The goal
is to make `spreads` smaller and more focused without losing the operator
workflow, persistent state, and product-specific controls it already has.

## Scope

This is about the live Alpaca options path, especially multi-leg option entries,
broker reconciliation, ledgers, risk admission, and account/runtime controls.
It is not a full design for every Nautilus adapter or every spreads feature.

Relevant Nautilus paths:

- `crates/model`
- `crates/data`
- `crates/execution`
- `crates/portfolio`
- `crates/live`
- `crates/backtest`
- `crates/risk`
- `crates/adapters/alpaca`
- `docs/developer_guide/alpaca_adapter_runtime_plan.md`
- `docs/developer_guide/alpaca_nuc_deployment.md`
- `docs/developer_guide/alpaca_live_options_roadmap.md`

Relevant spreads paths:

- `packages/core/services/execution`
- `packages/core/services/execution_intents`
- `packages/core/services/broker_sync.py`
- `packages/core/services/risk_manager.py`
- `packages/core/services/exit_manager.py`
- `packages/core/storage/execution_models.py`
- `packages/core/integrations/alpaca`
- `services/market_recorder.py`
- `docs/current_system_state.md`
- `docs/planning/2026-05-16_spreads_nautilus_integration_roadmap.md`
- `docs/planning/trading_engine_architecture.md`

## Executive Summary

Nautilus is the reusable trading-engine kernel. Its core value is typed market
and execution models, deterministic engine boundaries, venue adapters,
portfolio/account state, live/backtest runners, and broker reconciliation
patterns.

The current Alpaca layer is an experimental Rust options runtime rather than a
fully wired Python `TradingNode` adapter. That is intentional for the live
options path: it gives us fast control over Alpaca REST, multi-leg order
submission, account gates, candidate ledgers, outcome tracking, and operational
binaries while the standard adapter surface catches up.

`spreads` is not the engine kernel. It is the product and operations system:
FastAPI, Next.js, CLI, scheduler, Postgres read/write models, Redis queues,
market recorder, discovery workflows, and operator views. The right direction is
to keep those product boundaries and move duplicate broker/execution mechanics
behind a Nautilus runtime boundary.

The target is:

```text
spreads UI/API/CLI
        |
        v
spreads Postgres + control jobs + execution_intents
        |
        v
Nautilus Alpaca bridge or sidecar
        |
        v
Alpaca broker APIs
```

That makes `spreads` thinner by narrowing it to strategy policy, opportunity
selection, operator workflows, persistence projections, and reporting. Nautilus
owns validated order-list construction, broker submission, broker facts,
runtime-level admission gates, and execution lifecycle primitives.

## Design Invariants

These rules should be treated as implementation constraints, not preferences.

1. One broker submit path per migrated strategy family.

   If a strategy family is marked `nautilus`, it must not fall back to
   `alpaca_direct`. A failure to build or run the Nautilus handoff is a failed
   execution attempt, not permission to try another broker path.

2. One market quote stream owner in normal runtime.

   `spreads` should keep `services/market_recorder.py` as the normal Alpaca
   option quote websocket owner. Nautilus can consume sanitized quote snapshots
   from the handoff and may own trade updates or REST reconciliation, but it
   should not create a competing quote recorder unless the ownership model
   changes explicitly.

3. Selected candidates are evidence even when no broker order exists.

   A selected candidate that is blocked, rejected, expired, or intentionally
   tracked internally is still a strategy-quality observation. It should remain
   queryable separately from broker-filled PnL.

4. Product policy and engine safety are separate gates.

   `spreads` decides what the product wants to attempt. Nautilus performs final
   broker/runtime validation and may reject or reduce risk, but it must not
   loosen the policy snapshot that came from `spreads`.

5. Runtime facts are immutable; product projections are rebuildable.

   Broker responses, handoff payloads, rejection classes, fills, and runtime
   events should be append-only or otherwise auditable. Operator views and
   position projections can be rebuilt from those facts.

## Nautilus Base Architecture

Nautilus is organized around normalized, typed trading primitives and engine
boundaries. Adapter-specific code should translate venue APIs into those common
types instead of leaking broker-specific shapes throughout strategies.

### Core Layers

| Layer | Responsibility | Examples |
| --- | --- | --- |
| `crates/model` | Canonical trading model. | Instruments, option contracts, identifiers, prices, quantities, orders, positions, account and order events. |
| `crates/data` | Market data messages, requests, subscriptions, and engine plumbing. | Bars, quotes, trades, option chains, data requests. |
| `crates/execution` | Execution engine contracts and order lifecycle. | Submit/cancel/modify commands, execution clients, reconciliation, order reports, fill reports. |
| `crates/portfolio` | Account, position, balance, and portfolio state. | Position and account updates used by live and backtest paths. |
| `crates/risk` | Risk checks around submitted commands and account state. | Engine-level no-loosening controls. |
| `crates/live` | Live node/runtime orchestration. | Live data and execution client wiring. |
| `crates/backtest` | Deterministic simulation runtime. | Backtest engine and matching paths. |
| `crates/adapters/*` | Venue-specific translation and I/O. | Alpaca, Databento, Interactive Brokers, etc. |
| `crates/pyo3` and `crates/plugin` | Python and extension interfaces. | Bindings and plugin registration. |

The base pattern is:

```text
strategy/control decision
        |
        v
Nautilus command type
        |
        v
execution/data/portfolio engine boundary
        |
        v
adapter translation
        |
        v
venue API
        |
        v
normalized reports/events back into engine state
```

For option spreads, the important part is not just placing an Alpaca order. The
important part is preserving the typed lifecycle:

- Candidate or strategy decision.
- Validated `SubmitOrder` or `SubmitOrderList`.
- Broker-native order payload.
- Submitted parent and leg order identifiers.
- Order status snapshots.
- Fill reports and account activities.
- Position state and close state.
- Terminal outcome with enough context to evaluate the decision.

## Alpaca Runtime Architecture

The Alpaca crate lives at `crates/adapters/alpaca`. The implemented surface is
focused on US equity option workflows:

- Credential, endpoint, and feed configuration.
- Authenticated REST for account, positions, orders, option contracts, option
  snapshots, and account activities.
- Option order payload builders and local validation.
- Rust execution/runtime path for multi-leg option submission.
- REST reconciliation and trade-update handling.
- Candidate, outcome, performance, and strategy-state persistence.
- Operator binaries for scanning, submitting, validating, reporting, and
  runtime control.

The crate exports these major areas:

| Module | Role |
| --- | --- |
| `config`, `common`, `runtime_env` | Credentials, endpoints, runtime environment, account identity. |
| `http` | Alpaca REST client and response models. |
| `orders` | Alpaca simple and multi-leg option order payloads. |
| `submit` | Nautilus `SubmitOrder` and `SubmitOrderList` builders for Alpaca multi-leg orders. |
| `execution` | Execution client, admission helpers, order/fill report mapping, trade updates. |
| `strategy` | Scanner and candidate types for credit, debit, iron-condor, and naked-option workflows. |
| `options_runtime` | Runtime configuration, scanners, candidate selection, and ledger recording. |
| `options_engine` | Account-engine process loop for live Alpaca options runtime. |
| `management` | Close reasons, hold-time/expiration logic, and close pricing helpers. |
| `storage` | Async Postgres persistence for state, candidate ledger, market cache, outcome, and performance records. |
| `performance` | Entry performance and candidate outcome reporting. |
| `fleet` | Multi-account profile registry and fleet-level limits. |
| `websocket` | Alpaca trade-update websocket support. |

### Runtime Flow

The installed live process is intentionally thin at the binary edge. The library
owns the runtime loop so it can be tested and reused.

```text
systemd service
        |
        v
alpaca-options-runner
        |
        v
alpaca-options-engine
        |
        v
env files + TOML + fleet registry
        |
        v
OptionsEngineConfig
        |
        v
loop:
  load/reconcile strategy state
  manage open entries and close conditions
  enforce kill switch and entry window
  scan/select candidate
  apply account/risk/admission gates
  submit to broker or record dry-run candidate
  record events, candidate ledger, outcome, performance
```

The core loop is in `options_engine`. Strategy evaluation returns a
`StrategyDecision`:

- `Skip` for kill switch or entry-window blocks.
- `NoEntry` when no candidate qualifies.
- `RiskBlocked` when account-level caps block entries.
- `SelectedBlocked` when a candidate was selected but submit admission blocks
  broker action.
- `Selected` with `EntryMode::Submit` or `EntryMode::DryRun`.

The runtime configuration is in `OptionsEngineConfig`. It controls:

- Underlying universe.
- Enabled strategy families.
- Dry-run families.
- Max active entries.
- Max daily submissions.
- Max open broker orders.
- Per-underlying and per-sector limits.
- Quantity.
- Submit, manage, close, kill-switch, and force-flatten flags.
- Entry and close windows.
- Profit-target, stop-loss, max-hold, and expiration-exit rules.
- Candidate ledger and storage settings.
- Scanner configs.
- Fleet account identity and policy blocks.

### State And Ledgers

The Alpaca runtime has two state categories.

Local/runtime state:

- Current strategy entries.
- Parent order IDs.
- Submitted/closed/canceled flags.
- Close reason and close attempts.
- Daily submit counts used by risk gates.

Postgres ledgers:

- Candidate ledger.
- Candidate outcomes.
- Performance ledger.
- Strategy state records.
- Backtest market cache.

This distinction matters. Broker rejection or non-submission should not erase
the fact that the system saw and selected a candidate. The internal ledger must
track:

- Candidate discovered.
- Candidate selected.
- Candidate blocked by policy or account state.
- Candidate submitted.
- Broker accepted or rejected.
- Entry filled, partially filled, canceled, expired, or failed.
- Close attempted.
- Close filled, rejected, canceled, expired, or failed.
- Realized or simulated outcome.

That is how we evaluate whether the system is finding profitable trades even
when a broker blocks or rejects them.

### Deployment And Control Plane

The NUC deployment is user-level systemd plus config layers:

```text
env files:
  credentials, endpoints, account identity, emergency gates

TOML files:
  strategy, scanner, universe, risk, management, state, ledger

fleet registry:
  account roles, permissions, risk budgets, service names, paths

systemd:
  per-profile service processes

alpaca-control:
  status, accounts, scan, performance, fleet, health, logs
```

This is the correct operating shape for Nautilus-side live trading. It is not
necessarily the shape `spreads` should copy internally.

## Current Spreads Architecture

`spreads` is already structured as a product/control system:

- FastAPI backend.
- Next.js operator dashboard.
- Typer CLI.
- ARQ workers over Redis queues.
- Postgres as source of truth.
- Market recorder as the normal Alpaca option websocket owner.
- Discovery, opportunity scoring, execution intents, execution attempts,
  broker sync, session positions, risk manager, and exit manager.

The current system has important truth boundaries:

| Boundary | Current spreads owner | Notes |
| --- | --- | --- |
| Signal truth | Discovery/opportunity services | Candidate and opportunity state. |
| Execution handoff | `execution_intents` | Correct boundary for selecting `alpaca_direct` vs `nautilus`. |
| Order truth | `execution_attempts`, `execution_orders`, `execution_fills` | Broker-facing ledger and snapshots. |
| Position truth | `portfolio_positions`, `position_closes`, session positions | Product-facing attribution and lifecycle. |
| Market capture | `services/market_recorder.py` | Should remain the sole option websocket owner in normal runtime. |
| Operator truth | API/web read models | Should stay thin and projection-based. |

The existing Nautilus integration point is also in the right place:

- `packages/core/services/execution/runtimes.py` builds a Nautilus handoff.
- `packages/core/services/execution/nautilus_bridge.py` runs
  `alpaca-submit-order-list-bridge`.
- `packages/core/services/execution/__init__.py` chooses `NAUTILUS_RUNTIME` in
  `run_execution_submit`.
- Failures are fail-closed and recorded onto the execution attempt and linked
  execution intent.

That means `spreads` does not need a broad rewrite. It needs the bridge contract
made canonical, then duplicated broker mechanics removed one strategy family at
a time.

## Source Of Truth Rules

The main refinement is to avoid shared ownership of the same fact. Split facts
this way:

| Fact | Source of truth | Projection/consumer |
| --- | --- | --- |
| Opportunity and candidate selection | `spreads` discovery/opportunity tables | Nautilus handoff input. |
| Strategy policy snapshot | `spreads` execution intent | Nautilus validation input. |
| Handoff payload and hash | `spreads` execution attempt/intent payload | Nautilus bridge input; operator audit. |
| Broker submit side effect | Nautilus bridge/sidecar for migrated families | `spreads` receives runtime result. |
| Raw broker order snapshot | Nautilus runtime result or broker sync | `execution_orders`. |
| Raw broker fill/activity | Nautilus runtime result or broker sync | `execution_fills`, session positions. |
| Product position attribution | `spreads` portfolio/session position tables | API, web, reports. |
| Candidate/outcome analytics | Shared facts; product reports in `spreads` | Nautilus ledgers can remain independent until schemas converge. |

The practical rule: Nautilus should not directly mutate spreads tables through a
hidden path. It should return canonical facts through the bridge or sidecar, and
spreads should persist those facts deliberately.

## Responsibility Comparison

| Responsibility | Nautilus + Alpaca should own | Spreads should keep | Target decision |
| --- | --- | --- | --- |
| Operator UI/API/CLI | No | Yes | Keep in spreads. |
| Product configs and automation policy | Runtime validation only | Yes | Spreads owns policy, Nautilus enforces no-loosening limits. |
| Universe and opportunity selection | Reusable scanner/math may move over time | Yes | Keep selection in spreads until parity proves otherwise. |
| Market recorder websocket | Trade updates only if needed | Yes | Keep spreads as sole option market websocket owner. |
| Option symbol/order normalization | Yes | Minimal wrappers | Move duplicated broker-specific order shaping to Nautilus. |
| Multi-leg order construction | Yes | Handoff input only | Nautilus builds/validates final `SubmitOrderList` and broker payload. |
| Broker submission | Yes | Calls runtime boundary | Route migrated families through Nautilus only. |
| Broker rejection classification | Yes, canonical raw facts and reason classes | Product projection | Share canonical reason taxonomy. |
| Execution attempts ledger | Emits facts | Yes | Spreads remains product source of truth, stores Nautilus request/result. |
| Fills and order snapshots | Yes, canonical broker facts | Yes, projected tables | Nautilus returns facts; spreads persists projections. |
| Position lifecycle | Engine facts and close commands | Product attribution | Split by facts vs operator attribution. |
| Risk admission | Account/runtime hard gates | Strategy/product policy | Both, with Nautilus as final gate before broker. |
| Exit and close routing | Gradually yes | Initially yes | Port close path after open-entry parity. |
| Performance/outcome analytics | Candidate/outcome ledger primitives | Product reports | Spreads consumes Nautilus facts and keeps reporting views. |
| Fleet/account service ops | Yes for Nautilus services | Only account selection and display | Do not copy full fleet complexity into spreads. |

## What Spreads Should Not Copy

Do not copy the full Nautilus engine into `spreads`. That would make the system
larger, not smaller.

Avoid copying:

- Nautilus internal engine orchestration.
- Broad plugin and Python `TradingNode` adapter surfaces.
- A second market-data websocket owner for option quotes.
- Full multi-account fleet machinery unless spreads needs to manage those
  services directly.
- Duplicate Alpaca order payload builders once a strategy family is migrated.
- Duplicate close/reprice math after Nautilus close routing is proven.

`spreads` should call a smaller, stricter execution boundary. It should not
become another venue adapter.

## Tinier Spreads Target

The smaller target system has five parts:

1. Product control plane.

   Web, API, CLI, config, operator actions, deployment commands, runtime status,
   and alerts.

2. Discovery and opportunity state.

   Market recorder, discovery jobs, opportunity scoring, candidate policy, and
   strategy-specific selection. This remains where product iteration happens.

3. Execution intent boundary.

   `execution_intents` remains the handoff. It should contain the strategy
   decision, immutable policy snapshot, config hash, selected opportunity, and
   requested runtime.

4. Nautilus execution runtime.

   A bridge or sidecar accepts a versioned handoff, validates the order list,
   submits through Alpaca, reconciles broker facts, and returns canonical status.

5. Product projections and reports.

   `execution_attempts`, `execution_orders`, `execution_fills`,
   `portfolio_positions`, `position_closes`, runtime health, and performance
   read models stay in spreads Postgres.

Target diagram:

```text
operator
  |
  v
web / API / CLI
  |
  v
spreads Postgres read/write models
  ^
  |
jobs and control plane
  |
  v
execution_intents
  |
  v
Nautilus bridge or sidecar
  |
  v
Alpaca broker APIs

market_recorder -> spreads Postgres -> discovery/opportunities
```

## Recommended Implementation Plan

### Phase 0: Freeze Boundaries

Document and enforce these boundaries in spreads:

- `execution_intents` is the only automated execution handoff.
- `execution_attempts` is the product execution ledger.
- Nautilus runtime result is stored on the attempt/intent payload.
- No silent fallback from `nautilus` to `alpaca_direct`.
- Market recorder remains the normal option websocket owner.

Exit criteria:

- One page in spreads docs names these boundaries.
- A migrated strategy family cannot accidentally bypass Nautilus.
- Runtime selection is visible in API/CLI/operator views.

### Phase 1: Canonical Bridge Contract

Turn the current bridge payload into a versioned contract.

Input should include:

- `schema_version`.
- `execution_attempt_id`.
- `execution_intent_id`.
- `idempotency_key`.
- `handoff_hash`.
- `strategy_family`.
- `trade_intent` of `open` or `close`.
- `underlying_symbol`.
- `order_list_id`.
- Net limit price.
- Quantity.
- Legs with instrument ID, side, position intent, ratio quantity, and limit
  price source.
- Policy/config hash.
- Sanitized quote snapshot used for pricing.
- Requested runtime account/profile.
- Whether this is live, paper, or internal-only tracking.

Output should include:

- `schema_version`.
- `status`.
- `reason`.
- `idempotency_key`.
- `handoff_hash`.
- Parent broker order ID.
- Parent client order ID.
- Leg broker order IDs if known.
- Broker order snapshot if available.
- Raw broker rejection class and message.
- Runtime events.
- Reconciliation cursor or follow-up hint if submission status is unknown.
- Runtime version and config hash.

Exit criteria:

- Golden fixtures in both repos.
- Contract rejects missing leg prices, invalid ratios, and unsupported leg
  counts before broker submission.
- CLI bridge output is one parseable JSON object on success or failure.
- Replaying the same idempotency key cannot submit a duplicate broker order.

### Phase 2: Improve Outcome Ledger Integration

Use Nautilus candidate/outcome/performance concepts to make spreads reporting
better without moving all reporting immediately.

Persist these fields in spreads projections:

- Selected candidate identity.
- Handoff hash.
- Runtime name and version.
- Broker submit result.
- Broker rejection taxonomy.
- Filled average price by leg.
- Open mark, close mark, and mark source.
- Intended close reason.
- Actual terminal reason.
- Profit/loss by intent and by position.
- Whether the trade was real, rejected, blocked, or internally tracked only.

Exit criteria:

- A rejected or unsubmitted candidate remains visible as a selected opportunity.
- Reports separate strategy quality from broker permission/submission quality.
- Win rate and expectancy can be computed across real and faux/internal
  outcomes without mixing them as the same population.

Recommended status dimensions:

| Dimension | Values | Purpose |
| --- | --- | --- |
| `decision_state` | `discovered`, `selected`, `blocked`, `submitted` | Strategy decision quality. |
| `broker_state` | `not_submitted`, `accepted`, `rejected`, `filled`, `partial`, `canceled`, `expired`, `unknown` | Broker lifecycle. |
| `tracking_mode` | `live`, `paper`, `internal_only` | Separates real and faux populations. |
| `terminal_reason` | Stable enum plus raw broker text | Explains outcome without losing raw detail. |
| `pnl_source` | `broker_fill`, `mark_to_market`, `simulated`, `none` | Prevents mixed PnL math. |

### Phase 3: Risk And Sizing Split

Keep product-level sizing policy in spreads, then enforce hard runtime caps in
Nautilus.

Spreads should decide:

- Account/profile target.
- Strategy family.
- Max contracts requested.
- Product-specific allocation model.
- Whether the automation is allowed to request live execution.

Nautilus should enforce:

- Positive quantity and prices.
- Supported leg count.
- Broker/account permission blocks.
- Duplicate underlying/leg/open-order blocks.
- Max active entries.
- Max daily submits.
- Max open orders.
- Per-underlying and per-sector caps.
- No submit if kill switch or runtime policy blocks it.

Exit criteria:

- Quantity > 1 is allowed only through a policy snapshot.
- Nautilus can reduce or reject unsafe size, but cannot loosen spreads policy.
- Sizing decisions are visible in the intent, attempt, and runtime result.

### Phase 4: Open-Entry Vertical Slice

Migrate one strategy family completely before expanding. Recommended first slice:
`index_put_credit_entry`.

Flow:

```text
opportunity row + live quote snapshot
        |
        v
execution_intent
        |
        v
execution_attempt with runtime=nautilus
        |
        v
versioned Nautilus handoff
        |
        v
alpaca-submit-order-list-bridge
        |
        v
Alpaca
        |
        v
runtime_result stored on intent/attempt
        |
        v
broker_sync and session position projection
```

Exit criteria:

- Direct Alpaca submit path is disabled for this family.
- Broker rejection is classified and persisted.
- Parent and leg order IDs are persisted when available.
- Session position state matches broker facts after sync.
- Operator report shows selected, submitted, rejected, filled, and terminal
  counts.

### Phase 5: Close Routing

After open-entry parity, move close submission for the same family through
Nautilus.

Do this after open entries because close routing needs more correctness:

- Position quantity and remaining exposure.
- Leg direction reversal.
- Reduce-only behavior where supported.
- Mark freshness.
- Profit-target, stop-loss, force-close, expiration-risk reasons.
- Reprice and cooldown policy.

Exit criteria:

- Open and close for the first family use one runtime boundary.
- Close reason is preserved from decision to broker result.
- Close rejection does not corrupt position state.

### Phase 6: Reconciliation And Replay

Align replay/backtest evidence before scaling to more families.

Add:

- Deterministic handoff fixtures from real attempts.
- Broker snapshot replay fixtures.
- Outcome replay for rejected, accepted, partial fill, fill, cancel, expire, and
  close paths.
- Daily report comparing spreads projection with Nautilus runtime facts.

Exit criteria:

- Replaying an execution attempt produces the same handoff hash.
- Broker facts are idempotent.
- A report can explain every selected candidate's final state.

### Phase 7: Sidecar Upgrade

The subprocess bridge is acceptable now because it is explicit and fail-closed.
If latency, observability, or state sharing becomes a problem, move to a
long-running sidecar.

Preferred upgrade path:

1. Keep the versioned JSON contract stable.
2. Wrap the existing bridge behind a local HTTP or Unix-socket service.
3. Add health, version, config, and dry-run validation endpoints.
4. Keep the old CLI bridge as a diagnostic and fallback tool.

Do not switch to in-process bindings until the contract is stable. In-process
bindings make deployment simpler in some ways, but they also make dependency and
failure isolation worse.

### Phase 8: Delete Duplicate Paths

After each migrated family has proven open, close, sync, and reporting parity,
remove or quarantine duplicate code:

- Direct Alpaca order builders for that family.
- Duplicate leg-pricing shims.
- Duplicate close order generation.
- Runtime-specific branches that no longer need to exist.

Exit criteria:

- The codebase gets smaller after each migration.
- Runtime selection table is explicit about migrated and unmigrated families.
- Tests fail if a migrated family falls back to direct Alpaca.

## First Concrete Work Items In Spreads

1. Add a bridge contract module.

   Suggested path: `packages/core/services/execution/nautilus_contract.py`.
   It should build, validate, hash, and serialize versioned handoff payloads.
   `runtimes.py` can call this module instead of owning contract details.

2. Persist runtime request/result fields deliberately.

   Current runtime data is stored inside payload updates. Make it queryable
   enough for operator reports: runtime, handoff hash, handoff version, result
   status, result reason, broker rejection class, and parent order ID.

3. Add golden fixtures.

   Store one valid two-leg vertical, one invalid missing quote, one rejected
   broker response, and one submitted response. Use the same fixtures against
   Nautilus bridge parsing.

4. Add idempotency protection.

   The handoff hash should describe the economic request. The idempotency key
   should describe the intended broker side effect. Retries should be able to
   reconcile an existing order instead of submitting a duplicate.

5. Add a migrated-family guard.

   A config or registry should say which strategy families must use Nautilus.
   If one of those families reaches submit with `alpaca_direct`, fail before
   broker submission.

6. Keep market data ownership unchanged.

   Keep `services/market_recorder.py` as the normal option quote stream owner.
   Pass sanitized quote snapshots into the handoff.

7. Add an operator-facing runtime comparison report.

   The report should show selected candidates, broker attempts, rejection
   classes, fill quality, close quality, and internal-only outcomes side by side.
   This is what proves whether the strategy is good independent of broker
   permission issues.

## Open Decisions

| Decision | Recommendation |
| --- | --- |
| Subprocess bridge vs sidecar | Keep subprocess until the JSON contract is stable, then upgrade to sidecar if needed. |
| Where to store canonical outcome reports | Keep product reports in spreads; reuse Nautilus runtime facts and taxonomies. |
| Whether Nautilus should scan for spreads | Not yet. Keep spreads discovery until bridge/execution parity is proven. |
| Whether to share one Postgres schema | Avoid tight coupling. Store runtime facts in spreads projections and Nautilus ledgers separately until ownership is clearer. |
| How to handle assignment/exercise/expiration | Treat as broker lifecycle facts from Nautilus, projected into spreads positions. Add fixtures before live expansion. |
| How to handle unsupported strategy families | Keep them on `alpaca_direct` only if explicitly registered as unmigrated. No implicit fallback for migrated families. |

## Acceptance Criteria For The Target System

The target architecture is working when:

- One operator command can explain current runtime status.
- One report can explain selected, blocked, submitted, rejected, filled, closed,
  and expired trades.
- Rejected broker submissions still preserve selected-candidate evidence.
- Internal/faux outcome tracking is separated from real broker-filled PnL.
- A migrated family cannot silently use direct Alpaca submission.
- Spreads API/web are read-model and control surfaces, not broker adapter logic.
- Nautilus owns final order-list validation and broker-facing submission.
- Deleting duplicate direct Alpaca code reduces spreads complexity after each
  migrated family.

## Bottom Line

The best implementation is not to move all of Nautilus into `spreads`. The best
implementation is to make `spreads` a product shell and decision/control plane
that delegates broker mechanics to a strict Nautilus boundary.

Start with one vertical family, probably `index_put_credit_entry`, and make it
boringly complete:

- selected candidate persisted;
- versioned handoff persisted;
- Nautilus submit result persisted;
- broker facts reconciled;
- close path migrated;
- reports explain real and internally tracked outcomes separately;
- direct Alpaca path removed for that family.

That gives us a smaller spreads system, better broker correctness, and cleaner
evidence about whether the trading logic is actually profitable.
