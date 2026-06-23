//! Async Postgres persistence blocks for Alpaca runtime and ledgers.

#[cfg(feature = "live")]
pub const STORAGE_SCHEMA_DEFAULT: &str = "alpaca";

#[cfg(feature = "live")]
pub const STORAGE_ACCOUNT_ID_DEFAULT: &str = "default";

#[cfg(feature = "live")]
pub mod candidate_ledger;
#[cfg(feature = "live")]
pub mod market_cache;
#[cfg(feature = "live")]
pub mod performance;
#[cfg(feature = "live")]
pub mod state;

#[cfg(feature = "live")]
mod postgres;

#[cfg(feature = "live")]
pub use postgres::{StorageInitError, StorageMigrationStatus, StorageRepository};

#[cfg(feature = "live")]
pub use candidate_ledger::append_candidate_ledger_record;
#[cfg(feature = "live")]
pub use candidate_ledger::{
    CandidateLedgerSummaryFilters, read_candidate_ledger_records, summarize_candidate_ledger,
    summarize_candidate_ledger_records,
};

#[cfg(feature = "live")]
pub use market_cache::{read_backtest_market_cache, write_backtest_market_cache};

#[cfg(feature = "live")]
pub use performance::{
    CandidateOutcomeSummaryFilters, PerformanceLedgerSummaryFilters, append_candidate_outcome,
    append_performance_ledger_record, summarize_candidate_outcomes,
    summarize_candidate_outcomes_records, summarize_performance_ledger,
    summarize_performance_ledger_records,
};

#[cfg(feature = "live")]
pub use state::{load_strategy_state, load_strategy_state_record, save_strategy_state};

#[cfg(test)]
#[allow(dead_code)]
const _STORAGE_DOC_TEST: &str = "storage module";
