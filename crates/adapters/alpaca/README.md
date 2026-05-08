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
  dry-run scanning, multi-leg validation, candidate alerts, and performance reporting.

The Python `TradingNode` data and execution factories are still placeholders. Until those factories
are wired to live clients, document this work as an experimental Rust Alpaca options runtime rather
than a full Python live adapter.

The implementation plan is tracked in `docs/developer_guide/alpaca_adapter_runtime_plan.md`.
