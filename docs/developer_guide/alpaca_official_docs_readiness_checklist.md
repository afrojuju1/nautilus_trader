# Alpaca Official Docs Readiness Checklist

This checklist tracks the remaining work to make the Alpaca adapter and options runtime suitable
for official NautilusTrader documentation.

## Current readiness

- [ ] Decide the initial documentation status:
  - [ ] Experimental Rust Alpaca options runtime.
  - [ ] Full official Alpaca adapter alongside integrations such as Bybit and Interactive Brokers.
- [ ] Keep the public wording aligned with the implementation:
  - [ ] The Rust Alpaca options execution/runtime path is implemented and actively paper tested.
  - [ ] The Python `TradingNode` adapter factories are not implemented yet.
  - [ ] The opinionated options strategy engine is separate from the generic adapter surface.

## Public documentation surfaces

- [ ] Add `docs/integrations/alpaca.md`.
- [ ] Add Alpaca to the integrations index.
- [ ] Add `docs/api_reference/adapters/alpaca.md`.
- [ ] Add Alpaca to the adapter API reference index.
- [ ] Add `examples/live/alpaca/`.
- [ ] Add a clear status box to the public integration page.
- [ ] Add links from the Alpaca production/runtime developer docs to the public integration docs.

## Integration documentation content

- [ ] Document supported environments:
  - [ ] Paper trading endpoint.
  - [ ] Live trading endpoint.
  - [ ] Data endpoint selection.
  - [ ] Feed selection such as indicative, OPRA, IEX, SIP, or delayed SIP where applicable.
- [ ] Document credential resolution:
  - [ ] Explicit config values.
  - [ ] `APCA_API_KEY_ID`.
  - [ ] `APCA_API_SECRET_KEY`.
  - [ ] `ALPACA_API_KEY`.
  - [ ] `ALPACA_SECRET_KEY`.
- [ ] Document symbology:
  - [ ] OCC option symbols.
  - [ ] Nautilus instrument IDs on the `ALPACA` venue.
  - [ ] Supported underlying symbol forms.
- [ ] Document currently supported execution workflows:
  - [ ] Account queries.
  - [ ] Position queries.
  - [ ] Open order queries.
  - [ ] Order lookup.
  - [ ] Account activity polling.
  - [ ] Option contract loading.
  - [ ] Option snapshot loading.
  - [ ] Simple option limit payload validation/submission, if publicly supported.
  - [ ] Multi-leg option limit payload validation/submission.
  - [ ] Multi-leg parent and leg reconciliation.
  - [ ] Reduce-only close handling.
- [ ] Document unsupported or experimental behavior:
  - [ ] Full Python live data client.
  - [ ] Full Python live execution client.
  - [ ] Standard Python `TradingNode` Alpaca setup.
  - [ ] Streaming market data client.
  - [ ] Full historical market data client.
  - [ ] Equity order support, unless deliberately validated and documented.
  - [ ] Bracket, OCO, stop, stop-limit, and trailing stop support.
  - [ ] Assignment, exercise, and expiry as first-class fill/event models.
  - [ ] Full Alpaca API parity.

## API reference readiness

- [ ] Decide whether the API reference should describe only the implemented Rust/PyO3 runtime
      surface or wait until the Python adapter factories work.
- [ ] Document the available Alpaca Python package imports without implying unfinished factories
      are usable.
- [ ] Document the PyO3 scanner/runtime bindings that are intentionally public.
- [ ] Document config objects and clearly mark fields that are engine-specific rather than generic
      adapter configuration.
- [ ] Confirm generated stubs include the Alpaca module as intended.
- [ ] Run the docs build after adding the API page.

## Python adapter completion

- [ ] Implement or explicitly defer `AlpacaLiveDataClientFactory`.
- [ ] Implement or explicitly defer `AlpacaLiveExecClientFactory`.
- [ ] Wire Python config objects into real live clients if Alpaca is promoted to a full official
      adapter.
- [ ] Add a standard Python `TradingNode` example once the factories are implemented.
- [ ] Update package docstrings so they no longer describe completed Rust runtime work as only a
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
- [ ] Wire an Alpaca instrument provider into the standard Nautilus cache path if publishing as a
      normal adapter.
- [ ] Document option data entitlement requirements.
- [ ] Document any quote staleness, feed, or market-hours constraints.

## Examples

- [ ] Add a read-only credential/account status example.
- [ ] Add an option contract loading example.
- [ ] Add an option snapshot/scanner dry-run example.
- [ ] Add a multi-leg order validation example that does not submit orders.
- [ ] Add a paper-only options engine walkthrough with submission disabled by default.
- [ ] Add a safe paper smoke-test example that cancels accepted test orders.
- [ ] Add a standard `TradingNode` example after Python factories exist.
- [ ] Add docs or tests that verify public examples import and run in dry-run mode.

## Operational documentation

- [ ] Document config layering and precedence.
- [ ] Document environment override behavior.
- [ ] Document kill-switch defaults.
- [ ] Document submit/manage/close gates.
- [ ] Document systemd service installation and control commands.
- [ ] Document account role separation for main, directional, and undefined-risk paper accounts.
- [ ] Document operator status commands.
- [ ] Document fleet status commands.
- [ ] Document candidate alert commands.
- [ ] Document performance report commands.
- [ ] Document paper smoke-test policy.
- [ ] Document how to cancel accepted smoke-test orders.
- [ ] Document ledger locations and meanings.
- [ ] Document historical opportunity tracking and close-PnL accounting.

## Strategy runtime documentation

- [ ] Document that the options engine is opinionated strategy runtime, not the generic Alpaca
      adapter itself.
- [ ] Document each strategy family separately:
  - [ ] Put credit spreads.
  - [ ] Call credit spreads.
  - [ ] Iron condors.
  - [ ] Debit spreads.
  - [ ] Naked puts.
- [ ] Document shared risk gates:
  - [ ] Per-underlying limits.
  - [ ] Per-account limits.
  - [ ] Buying-power limits.
  - [ ] Minimum credit/debit gates.
  - [ ] Spread-width gates.
  - [ ] Quote-age gates.
  - [ ] Open-interest gates.
  - [ ] Earnings timing gates.
  - [ ] Management and close triggers.
- [ ] Document undefined-risk warnings and required explicit enablement.

## Tests and proof

- [ ] Add `tests/integration_tests/adapters/alpaca/` fixtures or an equivalent documented test
      home.
- [ ] Add fixture tests for config and env resolution.
- [ ] Add fixture tests for endpoint selection.
- [ ] Add fixture tests for option symbology conversion.
- [ ] Add fixture tests for option contract parsing.
- [ ] Add fixture tests for option snapshot parsing.
- [ ] Add fixture tests for order payload validation.
- [ ] Add fixture tests for multi-leg signed premium handling.
- [ ] Add fixture tests for order status mapping.
- [ ] Add fixture tests for fill/activity mapping.
- [ ] Add fixture tests for partial fills.
- [ ] Add fixture tests for rejected, canceled, expired, and replaced orders where applicable.
- [ ] Add fixture tests for startup reconciliation.
- [ ] Add fixture tests for terminal position reconciliation.
- [ ] Add docs example smoke/import tests.
- [ ] Keep live Alpaca credential tests out of normal CI unless explicitly configured.

## Stale documentation cleanup

- [ ] Update `crates/adapters/alpaca/README.md`.
- [ ] Update Rust crate-level docs in `crates/adapters/alpaca/src/lib.rs`.
- [ ] Update Rust config docs in `crates/adapters/alpaca/src/config.rs`.
- [ ] Update Python package docs in `nautilus_trader/adapters/alpaca/__init__.py`.
- [ ] Update Python config docs in `nautilus_trader/adapters/alpaca/config.py`.
- [ ] Update Python factory docs in `nautilus_trader/adapters/alpaca/factories.py`.
- [ ] Remove or qualify stale "planned", "scaffold", and "not implemented" wording wherever it no
      longer matches the Rust runtime.

## Packaging and release readiness

- [ ] Confirm Alpaca Python modules are included in built wheels.
- [ ] Confirm the `nautilus_pyo3.alpaca` module is intentionally exposed.
- [ ] Confirm docs references do not create broken links.
- [ ] Confirm examples do not require real credentials unless explicitly marked.
- [ ] Confirm secrets guidance is present and no real credentials are committed.
- [ ] Confirm public docs describe paper trading before live trading.

## Safety and limitations

- [ ] Add a paper-vs-live warning.
- [ ] Add options-risk warnings.
- [ ] Add undefined-risk warnings for naked strategies.
- [ ] Add a note that assignment, exercise, and expiry handling still need first-class lifecycle
      proof before being advertised as complete.
- [ ] Add a note that market-data capabilities depend on Alpaca account entitlements.
- [ ] Add a note that real smoke tests must cancel accepted orders unless intentionally left open.

## Proposed rollout

- [ ] Phase 1: publish experimental Alpaca docs.
- [ ] Phase 1: add public integration page with explicit limitations.
- [ ] Phase 1: add API reference placeholder or implemented-runtime API page.
- [ ] Phase 1: add safe examples.
- [ ] Phase 1: clean up stale scaffold language.
- [ ] Phase 2: implement Python `TradingNode` adapter factories.
- [ ] Phase 2: wire standard data/instrument provider behavior.
- [ ] Phase 2: add adapter integration fixtures and example smoke tests.
- [ ] Phase 3: broaden data, order, assignment/exercise/expiry, and historical-data support.
