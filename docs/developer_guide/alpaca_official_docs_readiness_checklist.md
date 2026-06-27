# Alpaca Official Docs Readiness Checklist

This checklist tracks the remaining work to make the Alpaca adapter and options runtime suitable
for official NautilusTrader documentation.

## Current readiness

- [x] Decide the initial documentation status:
  - [x] Experimental Rust Alpaca options runtime.
  - [ ] Full official Alpaca adapter alongside integrations such as Bybit and Interactive Brokers.
- [x] Keep the public wording aligned with the implementation:
  - [x] The Rust Alpaca options execution/runtime path is implemented and actively paper tested.
  - [x] The standard Python `TradingNode` path now covers exact option snapshot data and multi-leg
        option order lists, but Alpaca still should not be presented as a full official adapter.
  - [x] The Rust adapter has live data and execution factories, but public docs should not present
        Alpaca as a full official adapter yet.
  - [x] The opinionated options strategy engine is separate from the generic adapter surface.

## Public documentation surfaces

- [x] Add `docs/integrations/alpaca.md`.
- [x] Add Alpaca to the integrations index.
- [x] Add `docs/api_reference/adapters/alpaca.md`.
- [x] Add Alpaca to the adapter API reference index.
- [x] Add `examples/live/alpaca/`.
- [x] Add a clear status box to the public integration page.
- [x] Add links from the Alpaca production/runtime developer docs to the public integration docs.

## Integration documentation content

- [x] Document supported environments:
  - [x] Paper trading endpoint.
  - [x] Live trading endpoint.
  - [x] Data endpoint selection.
  - [x] Feed selection such as indicative, OPRA, IEX, SIP, or delayed SIP where applicable.
- [x] Document credential resolution:
  - [x] Explicit config values.
  - [x] `APCA_API_KEY_ID`.
  - [x] `APCA_API_SECRET_KEY`.
  - [x] `ALPACA_API_KEY`.
  - [x] `ALPACA_SECRET_KEY`.
  - [x] `ALPACA_API_SECRET`.
- [x] Document symbology:
  - [x] Alpaca/OCC option symbols.
  - [x] Nautilus instrument IDs on the `ALPACA` venue.
  - [x] Supported underlying symbol forms.
- [x] Document currently supported execution workflows:
  - [x] Account queries.
  - [x] Position queries.
  - [x] Open order queries.
  - [x] Order lookup.
  - [x] Account activity polling.
  - [x] Option contract loading.
  - [x] Option snapshot loading.
  - [x] Simple option limit payload validation/submission, if publicly supported.
  - [x] Multi-leg option limit payload validation/submission.
  - [x] Multi-leg parent and leg reconciliation.
  - [x] Reduce-only close handling.
- [x] Document unsupported or experimental behavior:
  - [x] Full Python live data client.
  - [x] Full Python live execution client.
  - [x] Standard Python `TradingNode` Alpaca setup.
  - [x] Streaming market data client.
  - [x] Full historical market data client.
  - [x] Equity order support, unless deliberately validated and documented.
  - [x] Bracket, OCO, stop, stop-limit, and trailing stop support.
  - [x] Assignment, exercise, and expiry as first-class fill/event models.
  - [x] Full Alpaca API parity.

## API reference readiness

- [x] Decide whether the API reference should describe only the implemented Rust/PyO3 runtime
      surface or wait until the Python adapter factories work.
- [x] Document the available Alpaca Python package imports without implying unfinished factories
      are usable.
- [x] Document the PyO3 scanner/runtime bindings that are intentionally public.
- [x] Document config objects and clearly mark fields that are engine-specific rather than generic
      adapter configuration.
- [x] Confirm generated stubs include the Alpaca module as intended.
- [x] Run the docs build after adding the API page.

## Python adapter completion

- [x] Implement or explicitly defer `AlpacaLiveDataClientFactory`.
- [x] Implement or explicitly defer `AlpacaLiveExecClientFactory`.
- [x] Wire option-capable clients into the standard Python node path if Alpaca is promoted to a full
      official adapter.
- [x] Add a standard Python `TradingNode` options example once option data and multi-leg execution
      are implemented.
- [x] Update package docstrings so they no longer describe completed Rust runtime work as only a
      scaffold.

## Market data and instrument provider gaps

- [ ] Define the official market-data scope for the first documented release:
  - [ ] Option contracts.
  - [ ] Option snapshots.
  - [ ] Quotes.
  - [ ] Trades.
  - [ ] Bars.
  - [ ] Greeks and implied volatility.
  - [ ] Underlying snapshots.
- [x] Add an active-risk option quote cache or explicitly document why REST snapshots are the
      supported quote path for the release.
- [ ] Add true real-time Alpaca option quote/trade streaming transport before treating quote
      freshness as a stronger live-trading dependency.
- [ ] Add entitlement-aware feed behavior for indicative versus OPRA option data.
- [x] Wire exact Alpaca option instrument loading into a standard Rust Nautilus data-client path.
- [x] Add exact option loading and snapshot/quote refresh to the standard node path if publishing
      as a normal adapter.
- [x] Document option data entitlement requirements.
- [x] Document current quote staleness, feed, and market-hours constraints.

## Examples

- [x] Add a read-only credential/account status example.
- [x] Add an option contract loading example.
- [x] Add an option snapshot/scanner dry-run example.
- [x] Add a multi-leg order validation example that does not submit orders.
- [x] Add a paper-only options engine walkthrough with submission disabled by default.
- [x] Add a safe paper smoke-test example that cancels accepted test orders.
- [x] Add a standard `TradingNode` options example after option data and multi-leg execution exist.
- [ ] Add docs or tests that verify public examples import and run in dry-run mode.

## Operational documentation

- [x] Document config layering and precedence.
- [x] Document environment override behavior.
- [x] Document kill-switch defaults.
- [x] Document submit/manage/close gates.
- [x] Document systemd service installation and control commands.
- [x] Document account role separation for main, directional, and undefined-risk paper accounts.
- [x] Document operator status commands.
- [x] Document fleet status commands.
- [x] Document candidate alert commands.
- [x] Document performance report commands.
- [x] Document paper smoke-test policy.
- [x] Document how to cancel accepted smoke-test orders.
- [x] Document ledger locations and meanings.
- [x] Document historical opportunity tracking and close-PnL accounting.
- [x] Document account capability preflight and options-level strategy gating.
- [x] Document assignment, exercise, expiry, and option non-trade activity polling once implemented.
- [x] Document historical option replay outputs once implemented.

## Strategy runtime documentation

- [x] Document that the options engine is opinionated strategy runtime, not the generic Alpaca
      adapter itself.
- [x] Document each strategy family separately:
  - [x] Put credit spreads.
  - [x] Call credit spreads.
  - [x] Iron condors.
  - [x] Debit spreads.
  - [x] Naked puts.
- [x] Document shared risk gates:
  - [x] Per-underlying limits.
  - [x] Per-account limits.
  - [x] Buying-power limits.
  - [x] Minimum credit/debit gates.
  - [x] Spread-width gates.
  - [x] Quote-age gates.
  - [x] Open-interest gates.
  - [x] Earnings timing gates.
  - [x] Management and close triggers.
- [x] Document undefined-risk warnings and required explicit enablement.

## Tests and proof

- [x] Add `tests/integration_tests/adapters/alpaca/` fixtures or an equivalent documented test
      home.
- [x] Add fixture tests for config and env resolution.
- [x] Add fixture tests for endpoint selection.
- [x] Add fixture tests for option symbology conversion.
- [x] Add fixture tests for option contract parsing.
- [x] Add fixture tests for option snapshot parsing.
- [x] Add fixture tests for order payload validation.
- [x] Add fixture tests for multi-leg signed premium handling.
- [x] Add fixture tests for order status mapping.
- [x] Add fixture tests for fill/activity mapping.
- [ ] Add fixture tests for option assignment, expiry, and option trade activity mapping.
- [x] Add fixture tests for partial fills.
- [x] Add fixture tests for rejected, canceled, expired, and replaced orders where applicable.
- [ ] Add fixture tests for startup reconciliation.
- [ ] Add fixture tests for terminal position reconciliation.
- [ ] Add fixture tests for account options trading-level and approval preflight.
- [ ] Add fixture tests for real-time option quote/trade stream parsing if the stream cache is
      included in the release scope.
- [ ] Add replay-harness tests for candidate outcomes against historical option data fixtures.
- [x] Add docs example smoke/import tests.
- [ ] Keep live Alpaca credential tests out of normal CI unless explicitly configured.

## Stale documentation cleanup

- [x] Update `crates/adapters/alpaca/README.md`.
- [x] Update Rust crate-level docs in `crates/adapters/alpaca/src/lib.rs`.
- [x] Update Rust config docs in `crates/adapters/alpaca/src/config.rs`.
- [x] Update Python package docs in `nautilus_trader/adapters/alpaca/__init__.py`.
- [x] Update Python config docs in `nautilus_trader/adapters/alpaca/config.py`.
- [x] Update Python factory docs in `nautilus_trader/adapters/alpaca/factories.py`.
- [x] Remove or qualify stale "planned", "scaffold", and "not implemented" wording wherever it no
      longer matches the Rust runtime.

## Packaging and release readiness

- [x] Confirm Alpaca Python modules are included in built wheels.
- [x] Confirm the `nautilus_pyo3.alpaca` module is intentionally exposed.
- [x] Confirm docs references do not create broken links.
- [x] Confirm examples do not require real credentials unless explicitly marked.
- [x] Confirm secrets guidance is present and no real credentials are committed.
- [x] Confirm public docs describe paper trading before live trading.

## Safety and limitations

- [x] Add a paper-vs-live warning.
- [x] Add options-risk warnings.
- [x] Add undefined-risk warnings for naked strategies.
- [x] Add a note that assignment, exercise, and expiry handling still need first-class lifecycle
      proof before being advertised as complete.
- [x] Add a note that market-data capabilities depend on Alpaca account entitlements.
- [x] Add a note that real smoke tests must cancel accepted orders unless intentionally left open.
- [ ] Add capability-level warnings for strategies that require higher Alpaca options approval.
- [ ] Add expiration-day and assignment-risk operator procedures before documenting undefined-risk
      strategies as production-ready.

## Proposed rollout

- [x] Phase 1: publish experimental Alpaca docs.
- [x] Phase 1: add public integration page with explicit limitations.
- [x] Phase 1: add API reference placeholder or implemented-runtime API page.
- [x] Phase 1: add safe examples.
- [x] Phase 1: clean up stale scaffold language.
- [ ] Phase 2: finish standard node option data and multi-leg execution parity.
- [ ] Phase 2: wire standard data/instrument provider behavior.
- [x] Phase 2: add adapter integration fixtures and example smoke tests.
- [x] Phase 3: add account capability preflight.
- [x] Phase 3: add assignment/exercise/expiry polling and lifecycle-risk handling.
- [x] Phase 3: add active-risk option quote cache and explicitly defer true streaming transport.
- [x] Phase 3: add historical option replay/research harness.
- [ ] Phase 3: broaden remaining data, order, and historical-data support.
