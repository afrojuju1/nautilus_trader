# Alpaca Live Options Roadmap

This roadmap tracks the next production work for the Alpaca options engine now that live
paper submissions are enabled and internal outcome tracking is in place.

The main objective is to improve realized and internally tracked trade quality through a
closed feedback loop:

```text
candidate -> selected -> submitted -> accepted/rejected -> filled -> managed -> closed
          -> realized/virtual outcome -> strategy and risk adjustment
```

The system should not rely on manual interpretation of raw logs. It should produce
operator-grade signals that identify which strategies, underlyings, entry windows, and close
rules are helping or hurting expectancy.

## Current Operating Assumptions

- Runtime profiles submit live paper orders unless explicitly disabled by config.
- Candidate, submit-result, strategy-state, performance-ledger, and candidate-outcome records are
  the source of truth for evaluation.
- Rejected or unfilled candidates still matter. They must stay visible in internal reporting so
  the operator can distinguish scanner quality from broker acceptance and fill quality.
- Win rate is not sufficient. Promotion decisions must use expectancy, average win, average loss,
  largest loss, rejection rate, and close reason.
- Fixed quantity is acceptable only as a temporary bridge. Sizing should become risk based before
  materially increasing exposure.

## Phase 1: Live Risk Governor

Add runtime breakers that can pause submissions without shutting down management or close logic.

Required controls:

- Max daily realized loss per account.
- Max daily hypothetical selected-outcome loss per account.
- Max consecutive losing closes per account and per strategy.
- Max consecutive rejected submissions per strategy, underlying, and rejection class.
- Max daily rejected submissions before pausing an account.
- Per-strategy and per-underlying pause state.
- Alerting when a breaker trips, including account, strategy, underlying, metric, threshold, and
  next allowed reset time.

Acceptance criteria:

- A breaker can pause new entries while existing positions remain managed.
- Breaker state survives process restarts.
- Breaker decisions are written to the candidate ledger or an equivalent structured operator log.
- `alpaca-control today` exposes active breaker state.

## Phase 2: Risk-Based Trade Sizing

Replace fixed `quantity` as the primary sizing mechanism with account-aware sizing.

Defined-risk spread sizing:

- Calculate max loss per spread from width, entry credit/debit, and multiplier.
- Size from configured account risk budget and remaining daily risk budget.
- Cap by per-trade risk, per-account open risk, per-underlying open risk, and daily loss breaker.
- Preserve hard minimum and maximum quantity limits in config.

Undefined-risk sizing:

- Size from buying power usage, delta exposure, and account-level undefined-risk budget.
- Require broker acceptance history before increasing quantity.
- Keep separate caps for naked calls and naked puts.
- Block sizing increases after same-day rejections or after a broker permission rejection class.

Acceptance criteria:

- Every selected candidate records intended quantity, calculated max loss or buying-power usage,
  sizing reason, and every cap that affected final quantity.
- Reports can compare fixed-size hypothetical outcomes with actual risk-sized outcomes.
- Quantity can be adjusted without redeploying code.

## Phase 3: Decision Quality Report

Build a daily report that ranks trade quality by the dimensions that actually drive decisions.

Required dimensions:

- Account.
- Strategy.
- Underlying.
- Entry window.
- Score bucket.
- Candidate rank.
- Selected/submitted/accepted/rejected/filled/closed status.
- Realized P/L.
- Virtual P/L.
- Close reason.
- Rejection reason.
- Win rate, average win, average loss, largest loss, and expectancy.

Acceptance criteria:

- One command prints a concise operator summary for today and for an explicit date range.
- The report separates broker-realized performance from internal virtual performance.
- The report highlights promote, demote, and pause candidates.
- The report includes enough detail to explain why a strategy is recommended for promotion or
  pause.

## Phase 4: Broker Rejection Feedback

Turn broker rejections into structured strategy feedback instead of noisy terminal failures.

Required behavior:

- Classify rejection reason into stable categories such as permission, buying power, order shape,
  invalid contract, market closed, price increment, and unknown.
- Record rejection category on the strategy entry and candidate outcome.
- Pause repeated rejection classes for the day.
- Exclude known unsupported order shapes from live submission until explicitly re-enabled.

Acceptance criteria:

- Rejected submissions are visible in the daily report with category and count.
- The system can pause only the failing strategy/underlying instead of pausing the whole account.
- Rejection handling does not remove internal virtual tracking for the same candidate class.

## Phase 5: Close Quality Improvements

Improve exit behavior with the same rigor used for entry selection.

Required analysis:

- Compare profit-target, stop-loss, max-hold, expiration-exit, manual, and force-flatten closes.
- Measure average P/L by close reason and strategy.
- Identify whether max-hold is adding value or simply closing stale trades.
- Track time-to-profit-target and time-to-stop-loss.

Potential runtime improvements:

- Earlier exit when quote deterioration exceeds a configured threshold.
- Dynamic max-hold by strategy and DTE.
- Separate close thresholds for put credit, call credit, iron condor, naked put, and naked call.
- Close reprice diagnostics so poor exits can be distinguished from poor entry selection.

Acceptance criteria:

- Reports show close reason expectancy.
- Close thresholds can be tuned by strategy.
- Virtual close outcomes are preserved and never overwritten after first close trigger.

## Phase 6: Promotion And Demotion Rules

Make strategy changes data-driven while still allowing operator override.

Promotion candidates should require:

- Minimum selected-outcome sample size.
- Positive selected-outcome expectancy.
- Acceptable largest loss.
- Average loss controlled relative to average win.
- Rejection rate below a configured threshold.
- No active risk-governor pause.

Demotion or pause candidates should trigger on:

- Negative expectancy after the minimum sample size.
- Rejection rate above threshold.
- Large-loss breach.
- Consecutive losing closes.
- Broker permission rejection.

Acceptance criteria:

- Reports recommend but do not silently change production config.
- Operator can apply a recommendation intentionally.
- Recommendation logic is deterministic and tested.

## Phase 7: Operational Workflow

Keep the live system easy to operate and easy to sync with upstream.

Required workflow:

- Before market open: check services, account status, active breakers, open orders, positions, and
  config.
- During market hours: monitor submissions, rejections, fills, and breaker state.
- After market close: run the decision quality report and performance report for the exact trade
  date.
- Before syncing upstream: commit local work, push the fork branch, fetch upstream, inspect
  divergence, and only then merge or rebase deliberately.

Acceptance criteria:

- The operator can answer "what happened today?" from one control command.
- The operator can answer "what should change tomorrow?" from one report.
- Upstream sync prep never mixes unrelated runtime config changes with repo code changes.
