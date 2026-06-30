DROP TABLE IF EXISTS market.quote_ticks_ts_init_rebuild;

CREATE TABLE market.quote_ticks_ts_init_rebuild
(
    ts_event UInt64,
    ts_init UInt64,
    init_time DateTime64(9, 'UTC') MATERIALIZED fromUnixTimestamp64Nano(toInt64(ts_init)),
    init_date Date MATERIALIZED toDate(init_time),
    event_time DateTime64(9, 'UTC') MATERIALIZED fromUnixTimestamp64Nano(toInt64(ts_event)),
    event_date Date MATERIALIZED toDate(event_time),
    instrument_id LowCardinality(String),
    venue LowCardinality(String),
    source LowCardinality(String),
    bid_price_raw Int128,
    ask_price_raw Int128,
    bid_size_raw UInt128,
    ask_size_raw UInt128,
    price_precision UInt8,
    size_precision UInt8,
    ingest_run_id UUID,
    inserted_at DateTime64(9, 'UTC') DEFAULT now64(9)
)
ENGINE = MergeTree
PARTITION BY toYYYYMMDD(init_date)
ORDER BY (source, ts_init, instrument_id, ts_event, ingest_run_id)
SETTINGS index_granularity = 8192;

INSERT INTO market.quote_ticks_ts_init_rebuild
(
    ts_event,
    ts_init,
    instrument_id,
    venue,
    source,
    bid_price_raw,
    ask_price_raw,
    bid_size_raw,
    ask_size_raw,
    price_precision,
    size_precision,
    ingest_run_id,
    inserted_at
)
SELECT
    ts_event,
    ts_init,
    instrument_id,
    venue,
    source,
    bid_price_raw,
    ask_price_raw,
    bid_size_raw,
    ask_size_raw,
    price_precision,
    size_precision,
    ingest_run_id,
    inserted_at
FROM market.quote_ticks;

RENAME TABLE
    market.quote_ticks TO market.quote_ticks_backup_002_event_date_layout,
    market.quote_ticks_ts_init_rebuild TO market.quote_ticks;
