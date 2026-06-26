# Warehouse Deployment

This directory owns the repo-level self-hosted market-data warehouse stack. It is not Alpaca
deployment plumbing; adapter services can depend on it later, but ClickHouse remains a shared
Nautilus analytical persistence service.

## Start ClickHouse

```bash
docker compose -f deploy/warehouse/compose.yml up -d clickhouse
```

To override defaults without committing secrets, create a local env file from
`warehouse.env.example` and pass it to Compose:

```bash
docker compose --env-file deploy/warehouse/warehouse.env -f deploy/warehouse/compose.yml up -d clickhouse
```

## Smoke Check

```bash
docker compose -f deploy/warehouse/compose.yml --profile smoke run --rm clickhouse-smoke
```

Expected output includes `warehouse_ok` and the running ClickHouse version.

## Database Ownership

The canonical market-data tables live in the `market` database, for example
`market.quote_ticks`. The warehouse operator stores migration metadata in
`warehouse.schema_migrations`; that database is control metadata, not the destination for quote,
trade, bar, or Greeks rows.

Keep `CLICKHOUSE_DB`/`CLICKHOUSE_DATABASE` set to `market` for normal local and self-hosted
operation. The migration command bootstraps both `market` and `warehouse` on an empty ClickHouse
instance.

## Useful Commands

```bash
docker compose -f deploy/warehouse/compose.yml ps
docker compose -f deploy/warehouse/compose.yml logs -f clickhouse
docker compose -f deploy/warehouse/compose.yml down
```

Ports bind to `127.0.0.1` by default:

- HTTP: `8123`
- Native TCP: `9000`

Persistent data and logs live in Docker volumes named by Compose. Use `down -v` only when you
intend to delete local warehouse data.
