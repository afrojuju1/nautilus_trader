CREATE DATABASE IF NOT EXISTS market;

CREATE TABLE IF NOT EXISTS market.quote_ticks
(
    ts_event UInt64,
    ts_init UInt64,
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
PARTITION BY toYYYYMMDD(event_date)
ORDER BY (instrument_id, ts_event, ts_init, source, ingest_run_id)
SETTINGS index_granularity = 8192;
