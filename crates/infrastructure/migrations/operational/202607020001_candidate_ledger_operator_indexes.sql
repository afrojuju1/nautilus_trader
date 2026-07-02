CREATE INDEX IF NOT EXISTS ix_candidate_ledger_account_type_ts
    ON candidate_ledger (account_id, record_type, ts_utc DESC);
