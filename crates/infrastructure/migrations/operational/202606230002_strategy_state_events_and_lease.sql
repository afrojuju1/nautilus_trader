ALTER TABLE strategy_state
    ADD COLUMN IF NOT EXISTS version BIGINT NOT NULL DEFAULT 0;

ALTER TABLE strategy_state
    ADD COLUMN IF NOT EXISTS writer_id TEXT;

ALTER TABLE strategy_state
    ADD COLUMN IF NOT EXISTS run_id UUID;

ALTER TABLE strategy_state
    ADD COLUMN IF NOT EXISTS last_event_id UUID;

CREATE INDEX IF NOT EXISTS ix_strategy_state_updated
    ON strategy_state (updated_at);

CREATE TABLE IF NOT EXISTS strategy_state_events (
    id BIGSERIAL PRIMARY KEY,
    account_id TEXT NOT NULL,
    event_id UUID NOT NULL,
    event_type TEXT NOT NULL,
    strategy TEXT,
    underlying TEXT,
    trade_date DATE,
    order_list_id TEXT,
    client_order_id TEXT,
    venue_order_id TEXT,
    ts_event TIMESTAMPTZ,
    ts_recorded TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    payload JSONB NOT NULL,
    CONSTRAINT uq_strategy_state_events_event UNIQUE (account_id, event_id)
);

CREATE INDEX IF NOT EXISTS ix_strategy_state_events_account_date
    ON strategy_state_events (account_id, trade_date, ts_recorded);

CREATE INDEX IF NOT EXISTS ix_strategy_state_events_order_list
    ON strategy_state_events (account_id, order_list_id);

CREATE INDEX IF NOT EXISTS ix_strategy_state_events_client_order
    ON strategy_state_events (account_id, client_order_id);

CREATE TABLE IF NOT EXISTS runtime_lease (
    account_id TEXT PRIMARY KEY,
    holder_id TEXT NOT NULL,
    run_id UUID NOT NULL,
    service_name TEXT,
    mode TEXT NOT NULL,
    acquired_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS ix_runtime_lease_expires
    ON runtime_lease (expires_at);
