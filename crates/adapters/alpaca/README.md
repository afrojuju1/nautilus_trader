# Nautilus Alpaca Adapter

This crate contains the Rust Alpaca Markets integration work for NautilusTrader. The current
implemented surface focuses on US equity option workflows:

- Shared configuration, credential, endpoint, and feed definitions.
- Authenticated Alpaca REST access for account, positions, orders, option contracts, option
  snapshots, and account activities.
- Alpaca option order payload builders and local validation for simple and multi-leg limit orders.
- A Rust execution/runtime path for option-spread submission, trade-update handling, and REST
  reconciliation.
- Diagnostic and operator binaries for paper-trading account checks, option-chain inspection,
  bounded scan comparison, multi-leg validation, candidate alerts, and performance reporting.

The Python `TradingNode` path exposes Alpaca data and execution factories for stock bars, exact
option snapshot quotes/Greeks, simple equity DAY limit orders, and option multi-leg DAY limit order
lists. This remains narrower than a full Alpaca adapter; document it as an experimental Alpaca
surface until paper/live proof is broader.

The implementation plan is tracked in `docs/developer_guide/alpaca_adapter_runtime_plan.md`.
