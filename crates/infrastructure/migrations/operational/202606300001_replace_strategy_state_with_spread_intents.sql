ALTER TABLE strategy_state RENAME TO strategy_state_snapshot_legacy;

CREATE TABLE strategy_state_account (
    account_id TEXT PRIMARY KEY,
    version BIGINT NOT NULL DEFAULT 0,
    writer_id TEXT,
    run_id UUID,
    last_event_id UUID,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO strategy_state_account (
    account_id,
    version,
    writer_id,
    run_id,
    last_event_id,
    updated_at,
    created_at
)
SELECT
    account_id,
    version,
    writer_id,
    run_id,
    last_event_id,
    updated_at,
    created_at
FROM strategy_state_snapshot_legacy
ON CONFLICT (account_id)
DO UPDATE SET
    version = EXCLUDED.version,
    writer_id = EXCLUDED.writer_id,
    run_id = EXCLUDED.run_id,
    last_event_id = EXCLUDED.last_event_id,
    updated_at = EXCLUDED.updated_at;

CREATE TABLE strategy_state (
    account_id TEXT NOT NULL,
    strategy_id TEXT NOT NULL,
    intent_id TEXT NOT NULL,
    trade_date DATE,
    underlying TEXT,
    status TEXT NOT NULL,
    spread_instrument_id TEXT,
    spread_raw_symbol TEXT,
    order_list_id TEXT,
    close_order_list_id TEXT,
    broker_parent_order_id TEXT,
    broker_close_parent_order_id TEXT,
    quantity BIGINT,
    submitted_at TIMESTAMPTZ,
    recorded_at TIMESTAMPTZ,
    closed_at TIMESTAMPTZ,
    payload JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (account_id, strategy_id, intent_id),
    FOREIGN KEY (account_id)
        REFERENCES strategy_state_account (account_id)
        ON DELETE CASCADE
);

CREATE INDEX ix_strategy_state_account_status
    ON strategy_state (account_id, status, trade_date);

CREATE INDEX ix_strategy_state_spread
    ON strategy_state (account_id, spread_instrument_id)
    WHERE spread_instrument_id IS NOT NULL;

CREATE INDEX ix_strategy_state_order_list
    ON strategy_state (account_id, order_list_id)
    WHERE order_list_id IS NOT NULL;

WITH entries AS (
    SELECT
        account_id,
        entry
    FROM strategy_state_snapshot_legacy
    CROSS JOIN LATERAL jsonb_array_elements(COALESCE(state -> 'entries', '[]'::jsonb)) AS entry
),
normalized AS (
    SELECT
        account_id,
        COALESCE(NULLIF(entry ->> 'strategy', ''), 'unknown') AS strategy_id,
        COALESCE(NULLIF(entry ->> 'spread_instrument_id', ''), NULLIF(entry ->> 'order_list_id', '')) AS intent_id,
        NULLIF(entry ->> 'trade_date', '')::date AS trade_date,
        NULLIF(entry ->> 'underlying', '') AS underlying,
        CASE
            WHEN COALESCE((entry ->> 'closed')::boolean, false) THEN 'closed'
            WHEN COALESCE((entry ->> 'canceled')::boolean, false) THEN 'canceled'
            WHEN COALESCE((entry ->> 'submitted')::boolean, false) THEN 'active'
            ELSE 'pending'
        END AS status,
        NULLIF(entry ->> 'spread_instrument_id', '') AS spread_instrument_id,
        NULLIF(entry ->> 'spread_raw_symbol', '') AS spread_raw_symbol,
        NULLIF(entry ->> 'order_list_id', '') AS order_list_id,
        NULLIF(entry ->> 'close_order_list_id', '') AS close_order_list_id,
        NULLIF(entry ->> 'parent_order_id', '') AS broker_parent_order_id,
        NULLIF(entry ->> 'close_parent_order_id', '') AS broker_close_parent_order_id,
        NULLIF(entry ->> 'quantity', '')::bigint AS quantity,
        NULLIF(entry ->> 'submitted_at_utc', '')::timestamptz AS submitted_at,
        NULLIF(entry ->> 'recorded_at_utc', '')::timestamptz AS recorded_at,
        NULLIF(entry ->> 'closed_at_utc', '')::timestamptz AS closed_at,
        entry AS payload
    FROM entries
)
INSERT INTO strategy_state (
    account_id,
    strategy_id,
    intent_id,
    trade_date,
    underlying,
    status,
    spread_instrument_id,
    spread_raw_symbol,
    order_list_id,
    close_order_list_id,
    broker_parent_order_id,
    broker_close_parent_order_id,
    quantity,
    submitted_at,
    recorded_at,
    closed_at,
    payload,
    updated_at
)
SELECT
    account_id,
    strategy_id,
    intent_id,
    trade_date,
    underlying,
    status,
    spread_instrument_id,
    spread_raw_symbol,
    order_list_id,
    close_order_list_id,
    broker_parent_order_id,
    broker_close_parent_order_id,
    quantity,
    submitted_at,
    recorded_at,
    closed_at,
    payload,
    NOW()
FROM normalized
WHERE intent_id IS NOT NULL;

CREATE TABLE strategy_broker_leg_evidence (
    account_id TEXT NOT NULL,
    strategy_id TEXT NOT NULL,
    intent_id TEXT NOT NULL,
    evidence_id TEXT NOT NULL,
    evidence_type TEXT NOT NULL,
    symbol TEXT,
    instrument_id TEXT,
    ratio BIGINT,
    broker_order_id TEXT,
    client_order_id TEXT,
    order_list_id TEXT,
    side TEXT,
    quantity NUMERIC,
    status TEXT,
    payload JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (account_id, strategy_id, intent_id, evidence_id),
    FOREIGN KEY (account_id, strategy_id, intent_id)
        REFERENCES strategy_state (account_id, strategy_id, intent_id)
        ON DELETE CASCADE
);

CREATE INDEX ix_strategy_broker_leg_evidence_order
    ON strategy_broker_leg_evidence (account_id, broker_order_id)
    WHERE broker_order_id IS NOT NULL;

CREATE INDEX ix_strategy_broker_leg_evidence_client_order
    ON strategy_broker_leg_evidence (account_id, client_order_id)
    WHERE client_order_id IS NOT NULL;

CREATE INDEX ix_strategy_broker_leg_evidence_symbol
    ON strategy_broker_leg_evidence (account_id, symbol)
    WHERE symbol IS NOT NULL;

WITH normalized AS (
    SELECT
        account_id,
        strategy_id,
        intent_id,
        order_list_id,
        broker_parent_order_id,
        broker_close_parent_order_id,
        close_order_list_id,
        payload AS entry
    FROM strategy_state
),
spread_legs AS (
    SELECT
        account_id,
        strategy_id,
        intent_id,
        'spread_leg:' || COALESCE(NULLIF(leg ->> 'symbol', ''), NULLIF(leg ->> 'instrument_id', ''), ordinal::text) AS evidence_id,
        'spread_leg' AS evidence_type,
        NULLIF(leg ->> 'symbol', '') AS symbol,
        NULLIF(leg ->> 'instrument_id', '') AS instrument_id,
        NULLIF(leg ->> 'ratio', '')::bigint AS ratio,
        NULL::text AS broker_order_id,
        NULL::text AS client_order_id,
        order_list_id,
        jsonb_build_object('source', 'strategy_state.spread_legs', 'leg', leg) AS payload
    FROM normalized
    CROSS JOIN LATERAL jsonb_array_elements(COALESCE(entry -> 'spread_legs', '[]'::jsonb)) WITH ORDINALITY AS legs(leg, ordinal)
),
strategy_legs AS (
    SELECT
        account_id,
        strategy_id,
        intent_id,
        'strategy_leg:' || leg_key || ':' || symbol AS evidence_id,
        'strategy_leg' AS evidence_type,
        symbol,
        NULL::text AS instrument_id,
        NULL::bigint AS ratio,
        NULL::text AS broker_order_id,
        NULL::text AS client_order_id,
        order_list_id,
        jsonb_build_object('source', leg_key) AS payload
    FROM normalized
    CROSS JOIN LATERAL (
        VALUES
            ('short_symbol', NULLIF(entry ->> 'short_symbol', '')),
            ('long_symbol', NULLIF(entry ->> 'long_symbol', '')),
            ('short_call_symbol', NULLIF(entry ->> 'short_call_symbol', '')),
            ('long_call_symbol', NULLIF(entry ->> 'long_call_symbol', ''))
    ) AS legs(leg_key, symbol)
    WHERE symbol IS NOT NULL
),
parent_orders AS (
    SELECT
        account_id,
        strategy_id,
        intent_id,
        'entry_parent_order:' || broker_parent_order_id AS evidence_id,
        'entry_parent_order' AS evidence_type,
        NULL::text AS symbol,
        NULL::text AS instrument_id,
        NULL::bigint AS ratio,
        broker_parent_order_id AS broker_order_id,
        order_list_id AS client_order_id,
        order_list_id,
        jsonb_build_object('source', 'strategy_state.parent_order_id', 'broker_order_id', broker_parent_order_id, 'order_list_id', order_list_id) AS payload
    FROM normalized
    WHERE broker_parent_order_id IS NOT NULL
),
close_parent_orders AS (
    SELECT
        account_id,
        strategy_id,
        intent_id,
        'close_parent_order:' || broker_close_parent_order_id AS evidence_id,
        'close_parent_order' AS evidence_type,
        NULL::text AS symbol,
        NULL::text AS instrument_id,
        NULL::bigint AS ratio,
        broker_close_parent_order_id AS broker_order_id,
        close_order_list_id AS client_order_id,
        close_order_list_id AS order_list_id,
        jsonb_build_object('source', 'strategy_state.close_parent_order_id', 'broker_order_id', broker_close_parent_order_id, 'close_order_list_id', close_order_list_id) AS payload
    FROM normalized
    WHERE broker_close_parent_order_id IS NOT NULL
),
all_evidence AS (
    SELECT * FROM spread_legs
    UNION ALL
    SELECT * FROM strategy_legs
    UNION ALL
    SELECT * FROM parent_orders
    UNION ALL
    SELECT * FROM close_parent_orders
)
INSERT INTO strategy_broker_leg_evidence (
    account_id,
    strategy_id,
    intent_id,
    evidence_id,
    evidence_type,
    symbol,
    instrument_id,
    ratio,
    broker_order_id,
    client_order_id,
    order_list_id,
    payload,
    updated_at
)
SELECT
    account_id,
    strategy_id,
    intent_id,
    evidence_id,
    evidence_type,
    symbol,
    instrument_id,
    ratio,
    broker_order_id,
    client_order_id,
    order_list_id,
    payload,
    NOW()
FROM all_evidence;

DROP TABLE strategy_state_snapshot_legacy;
