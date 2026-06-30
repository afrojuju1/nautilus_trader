//! Operational Postgres storage for live strategy control-plane state and ledgers.

pub const OPERATIONAL_SCHEMA_DEFAULT: &str = "trading_ops";
pub const OPERATIONAL_ACCOUNT_ID_DEFAULT: &str = "default";
pub const STRATEGY_STATE_MIGRATION_VERSION: i64 = 202606300001;

mod postgres;

pub mod candidate_ledger;
pub mod lease;
pub mod performance_ledger;
pub mod state;

pub use candidate_ledger::{
    CandidateLedgerSummaryFilters, append_candidate_ledger_record, read_candidate_ledger_records,
};
pub use lease::{
    RuntimeLeaseRequest, RuntimeLeaseStatus, acquire_runtime_lease, heartbeat_runtime_lease,
    release_runtime_lease,
};
pub use performance_ledger::{
    CandidateOutcomeSummaryFilters, PerformanceLedgerSummaryFilters, append_candidate_outcome,
    append_performance_ledger_payload, read_candidate_outcome_records,
    read_performance_ledger_records,
};
pub use postgres::{OperationalInitError, OperationalMigrationStatus, OperationalRepository};
pub use state::{
    StrategyStateIntentSummary, StrategyStateMetadata, StrategyStateMutation,
    StrategyStateWriteResult, StrategyStateWriteStatus, load_strategy_state,
    load_strategy_state_intent_payloads, load_strategy_state_intent_summary,
    load_strategy_state_metadata,
    load_strategy_state_record, persist_strategy_state_mutation, save_strategy_state,
};

#[cfg(test)]
#[allow(dead_code)]
const _OPERATIONAL_DOC_TEST: &str = "operational module";
