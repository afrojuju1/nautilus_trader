//! Market-data warehouse persistence backends.

#[cfg(any(
    feature = "clickhouse",
    feature = "clickhouse-write",
    feature = "clickhouse-read",
    feature = "clickhouse-catalog",
    feature = "clickhouse-migrations",
    feature = "live-dual-write",
))]
pub mod clickhouse;
#[cfg(feature = "live-dual-write")]
pub mod live;
