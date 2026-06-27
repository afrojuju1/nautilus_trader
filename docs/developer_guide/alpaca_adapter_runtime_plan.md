# Alpaca Adapter Runtime Plan

This document captures the proposed path for building an Alpaca Markets adapter in
NautilusTrader and migrating a strategy equivalent to `put_credit`.

## Goal

Build a Nautilus-native Alpaca adapter that can run US equity and US equity option workflows in
paper trading first, with enough live-data and execution parity to support short-dated put credit
spread automation.

Public documentation readiness is tracked in
[Alpaca Official Docs Readiness Checklist](alpaca_official_docs_readiness_checklist.md). The
current public-facing integration page is [Alpaca](../integrations/alpaca.md), which documents the
implemented Rust options runtime as experimental until the standard node path covers option data,
multi-leg execution, and operator lifecycle parity.

The first target strategy mirrors the current spreads workflow:

- Underlyings: SPY, QQQ, IWM, DIA, GLD.
- Cadence: five minutes during the 09:45-14:30 ET entry window.
- Structure: put credit spreads with 5-10 DTE.
- Selection: short delta around 0.18-0.28, widths 2/3/5, min open interest, quote age, spread,
  POP/EV/slippage/IV scoring, and minimum return-on-risk gates.
- Execution: paper account, multi-leg net-credit limit orders, position-aware risk gates, and
  target/stop exit management.

## Adapter Shape

Follow Nautilus' existing live adapter pattern:

- Rust crate: `crates/adapters/alpaca`.
- Python package: `nautilus_trader/adapters/alpaca`.
- Config and factories exposed in Python.
- Rust owns HTTP/WebSocket clients, wire models, parsing, retries, rate limits, and execution
  reconciliation.
- Python remains the user-facing configuration and strategy assembly surface.

The scaffold added with this plan registers `nautilus-alpaca` and exposes venue/config constants.
The Rust crate now includes live data and execution client factories; the Python package remains the
user-facing assembly surface for the narrower equity path while options continue to migrate.

The first implementation slices add authenticated REST access, an option-contract provider for
`/v2/options/contracts`, and batched option snapshot loading through
`/v1beta1/options/snapshots`. Credentials are resolved from explicit config values first, then
from `APCA_API_KEY_ID`/`APCA_API_SECRET_KEY`, then from the existing deployment aliases
`ALPACA_API_KEY`/`ALPACA_SECRET_KEY`.
The contract provider can also convert Alpaca option contract payloads into Nautilus
`OptionContract` instruments on venue `ALPACA`.
The Rust `AlpacaDataClient` and factory can exact-load those option instruments through the standard
Nautilus live data-client request/subscription surface. The active scanner path is the
Nautilus-native option-chain actor and bounded comparison command; the old REST-only put-credit
dry-run binary has been retired.
The next runtime slice adds account, position, and open-order polling through Alpaca Trading REST
plus a non-submitting multi-leg order payload builder/validator for paper put credit spread
payloads.
Paper order submission and cancel requests are now exposed through the REST client; the smoke
binary submits one MLeg order and cancels it immediately if Alpaca accepts it.
Order-by-ID polling, account-activity polling, admission gates for duplicate option exposure, a
paper execution lifecycle harness, and a Nautilus timer strategy scaffold now share the same put
credit scanner logic.

## Alpaca API Mapping

Market data:

- Contracts: load option contracts from Alpaca's option contracts endpoint by underlying,
  expiration, type, status, and style.
- Snapshots: use option snapshots for quotes, greeks, IV, latest trade, and underlying snapshots.
- Historical bars/trades/quotes: support backfill and replay inputs.
- WebSocket: subscribe to explicit option symbols only; maintain a dynamic subscription set for
  the candidate chain because wildcard option quote subscriptions are not available.
- Feeds: support `indicative` first and `opra` as a paid-feed switch.

Execution:

- Account and positions: poll account, positions, and orders for startup reconciliation and
  periodic repair. (Initial account/position/order polling complete; order-by-ID and activity
  polling are available for lifecycle reconciliation.)
- Trade updates: consume the account trade update stream for order state changes.
- Multi-leg orders: build, validate, and submit Alpaca `order_class="mleg"` payloads with signed
  net limit prices. (Initial paper submission and cancel path complete.)
- Admission: block new entries when the account is blocked, candidate legs already have open
  positions, or working orders already reference the same option underlying.
- Reconciliation: activity polling must cover fills, corrections, assignments, exercises, and
  expirations that may not be fully represented by order events. (Initial `FILL`, `OPASN`,
  `OPEXP`, `OPEXC`, and `OPTRD` polling path complete.)

## Instrument Model

The adapter should model:

- Equity underlyings as Nautilus equity instruments on venue `ALPACA`.
- Listed option contracts as Nautilus options with canonical Alpaca option symbols.
- Put credit spreads as strategy-generated multi-leg order instructions, not separate synthetic
  instruments for the first implementation.

Keep symbology conversion isolated so the strategy can reason in Nautilus `InstrumentId`s while
the adapter submits Alpaca symbols.

## Runtime Architecture

Phase 1:

- Implement authenticated REST client. (Initial option-contract path complete.)
- Implement contract provider for equity option instruments. (Initial Alpaca contract model and
  Nautilus `OptionContract` conversion complete.)
- Implement native live data-client factory for option instruments. (Exact `InstrumentId` loading and
  cached instrument replay complete.)
- Implement latest option snapshot/quote request path. (Initial batched snapshot path complete.)
- Implement account/position/order polling. (Initial REST polling complete.)
- Implement paper multi-leg order submission. (Initial direct submit/cancel smoke path complete.)
- Implement paper-only multi-leg payload validation. (Initial put credit spread payload builder
  complete; submit is exposed separately through the REST client.)
- Add a dry-run strategy harness that emits candidate decisions without orders. (Initial standalone
  scanner complete; scanner logic is shared by a timer-style Rust loop and a Nautilus
  `AlpacaPutCreditStrategy` scaffold.)

Phase 2:

- Add option WebSocket data client with explicit subscriptions.
- Add trade update stream execution reconciliation.
- Add order modify/cancel support.
- Add snapshot Greek/IV refresh loop for scoring inputs.

Phase 3:

- Port the `put_credit` selection logic into a Nautilus `Strategy`.
- Replace the transitional strategy subprocess scanner call with direct Python bindings once the
  Alpaca Rust scanner/client is exposed through PyO3.
- Add target/stop exit policy.
- Add historical decision replay using Alpaca bars/snapshots where available.
- Run paper soak with order submission disabled, then paper execution enabled.

Initial Phase 3 runner:

- `alpaca-options-engine` scans configured underlyings, applies account/position/open-order
  admission checks, enforces daily duplicate-entry state, selects one candidate, and can submit a
  Nautilus `SubmitOrderList` through the Alpaca execution client when
  TOML `runtime.submit = true` or `ALPACA_SUBMIT=true`.
- The same runner can scan Phase 7A `call_credit` candidates by setting
  `ALPACA_STRATEGIES=call` or scan both vertical-credit directions with `both`.
- The runner evaluates stale entry cancellation and close triggers from persisted state. Broker
  management actions are disabled unless TOML `runtime.manage = true` or `ALPACA_MANAGE=true`;
  close order submission also requires TOML `runtime.close = true` or `ALPACA_CLOSE=true`.
- Management triggers include profit target, stop-loss debit, max hold, expiration-risk exit,
  force-flatten, stale entry cancellation, and a kill switch for blocking new entries.
- Strategy/scanner/management parameters live in `ALPACA_CONFIG_PATH`, defaulting to
  `~/.config/nautilus-trader/alpaca/options-engine.toml`; env remains for secrets, endpoints, and
  emergency runtime overrides.
- Submission is disabled by default so paper soak can run safely before enabling execution.
- Python strategy submission remains blocked by the current Python `OrderList` invariant that all
  orders share one `InstrumentId`; Alpaca MLeg entries require distinct option leg instruments, so
  the first native submit runner is Rust-side.

## Strategy Migration

The Nautilus strategy should not copy the current app's job scheduler or alerting layer. It should
only port the trading decision:

1. On timer, load the eligible underlyings and expiration window.
2. Request chain contracts and latest option snapshots.
3. Build candidate put credit spreads from the chain.
4. Score candidates using the existing POP/EV/slippage/IV/ROR rules.
5. Check account and portfolio risk.
6. Submit one multi-leg net-credit limit order when all gates pass.
7. Manage exits from Nautilus position/order events and periodic account reconciliation.

The existing `spreads` system can remain the operator UI, alerting, and policy research layer until
Nautilus has equivalent operational visibility.

## Key Risks

- Alpaca option WebSocket subscriptions require explicit symbols, so chain filtering must happen
  before live quote subscription.
- OPRA access changes data quality materially; `indicative` is acceptable for development but not
  a final live-trading assumption.
- Options assignment, exercise, expiry, and corporate action handling need activity reconciliation,
  not just order event handling.
- Nautilus has strong primitives, but portfolio/risk behavior for broker multi-leg option spreads
  needs targeted paper tests before live trading.
- The current strategy depends on snapshot Greeks and IV; if these are missing or stale, the
  Nautilus strategy must gate entries rather than substitute weak estimates.

## Validation Gates

- `cargo check -p nautilus-alpaca --no-default-features`.
- Python import smoke for `nautilus_trader.adapters.alpaca`.
- REST client unit tests against captured Alpaca fixtures.
- Paper-only contract load for SPY and QQQ options.
- Paper-only multi-leg order dry run that validates payload shape but does not submit.
- Submit one tiny paper spread in market hours, verify order lifecycle through REST plus trade
  update stream, then cancel/close and reconcile positions.
