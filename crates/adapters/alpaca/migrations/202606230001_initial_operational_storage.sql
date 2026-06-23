CREATE TABLE IF NOT EXISTS strategy_state (
    account_id TEXT PRIMARY KEY,
    state JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS candidate_ledger (
    id BIGSERIAL PRIMARY KEY,
    account_id TEXT NOT NULL,
    trade_date DATE NOT NULL,
    ts_utc TIMESTAMPTZ NOT NULL,
    record_type TEXT NOT NULL,
    alert_type TEXT,
    severity TEXT,
    alert_key TEXT,
    migration_key TEXT,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE candidate_ledger
    ADD COLUMN IF NOT EXISTS migration_key TEXT;

CREATE INDEX IF NOT EXISTS ix_candidate_ledger_account_date
    ON candidate_ledger (account_id, trade_date, ts_utc);

CREATE UNIQUE INDEX IF NOT EXISTS ux_candidate_ledger_account_migration_key
    ON candidate_ledger (account_id, migration_key)
    WHERE migration_key IS NOT NULL;

CREATE TABLE IF NOT EXISTS performance_ledger (
    id BIGSERIAL PRIMARY KEY,
    account_id TEXT NOT NULL,
    ledger_date DATE NOT NULL,
    ts_utc TIMESTAMPTZ NOT NULL,
    record_key TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT uq_performance_ledger_record_key UNIQUE (account_id, record_key)
);

CREATE INDEX IF NOT EXISTS ix_performance_ledger_account_date
    ON performance_ledger (account_id, ledger_date, ts_utc);

CREATE TABLE IF NOT EXISTS candidate_outcome (
    id BIGSERIAL PRIMARY KEY,
    account_id TEXT NOT NULL,
    trade_date DATE NOT NULL,
    ts_utc TIMESTAMPTZ NOT NULL,
    record_key TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT uq_candidate_outcome_record_key UNIQUE (account_id, record_key)
);

CREATE INDEX IF NOT EXISTS ix_candidate_outcome_account_date
    ON candidate_outcome (account_id, trade_date, ts_utc);

CREATE TABLE IF NOT EXISTS backtest_market_cache (
    id BIGSERIAL PRIMARY KEY,
    account_id TEXT NOT NULL,
    cache_kind TEXT NOT NULL,
    cache_key TEXT NOT NULL,
    payload JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT uq_backtest_market_cache_key UNIQUE (account_id, cache_kind, cache_key)
);

CREATE INDEX IF NOT EXISTS ix_backtest_market_cache_account_kind
    ON backtest_market_cache (account_id, cache_kind);
