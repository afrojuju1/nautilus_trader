# Alpaca Operational State Runbook

This runbook covers the operational-state replacement for the Alpaca options runtime.

Postgres is the operational source of truth:

- `strategy_state`: one normalized row per account, strategy, and spread/order intent.
- `strategy_state_account`: account-level version, writer, run, and last-event metadata.
- `strategy_broker_leg_evidence`: separate leg and broker-order evidence for each intent.
- `strategy_state_events`: append-only mutation history.

The local JSON state file is only a recovery mirror for non-Postgres bootstrap and disaster
inspection.

## Backup Before Migration

Stop the trading engine first if the market is open or if you want a quiet state snapshot:

```bash
docker compose --env-file .env -f deploy/alpaca/compose.yml --profile engine stop alpaca-options
```

Back up the container Postgres operational schema:

```bash
mkdir -p backups/alpaca-operational
docker compose --env-file .env -f deploy/alpaca/compose.yml exec -T postgres \
  sh -lc 'pg_dump -U "$POSTGRES_USER" -d "$POSTGRES_DB" -n trading_ops --format=custom' \
  > "backups/alpaca-operational/trading_ops-$(date -u +%Y%m%dT%H%M%SZ).dump"
```

Back up the container state volume too:

```bash
docker run --rm \
  -v nautilus-alpaca_alpaca-state:/state:ro \
  -v "$PWD/backups/alpaca-operational:/backup" \
  debian:bookworm-slim \
  tar czf "/backup/alpaca-state-$(date -u +%Y%m%dT%H%M%SZ).tar.gz" -C /state .
```

For a non-container local Postgres, source the same repo `.env` you use to run the service and dump
the configured operational schema:

```bash
set -a
. ./.env
set +a
pg_dump "$NAUTILUS_OPERATIONAL_DATABASE_URL" \
  -n "${NAUTILUS_OPERATIONAL_SCHEMA:-trading_ops}" \
  --format=custom \
  > "backups/alpaca-operational/local-trading_ops-$(date -u +%Y%m%dT%H%M%SZ).dump"
```

## Apply

Start or recreate the runtime through compose. The operational repository applies `sqlx`
migrations on startup:

```bash
docker compose --env-file .env -f deploy/alpaca/compose.yml --profile engine up -d --force-recreate alpaca-options
```

Validate that migration `202606300001` is applied and that the replacement rows were backfilled:

```bash
docker exec nautilus-alpaca-alpaca-options-1 nautilus adapters alpaca status --json
docker compose --env-file .env -f deploy/alpaca/compose.yml exec -T postgres \
  sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c "
    SELECT MAX(version) AS latest_migration
    FROM trading_ops._sqlx_migrations
    WHERE success;
    SELECT status, COUNT(*)
    FROM trading_ops.strategy_state
    GROUP BY status
    ORDER BY status;
    SELECT evidence_type, COUNT(*)
    FROM trading_ops.strategy_broker_leg_evidence
    GROUP BY evidence_type
    ORDER BY evidence_type;
  "'
```

Expected operator signs:

- `operational_store.latest_migration_version` is at least `202606300001`.
- `strategy_state.spread_intents` is non-null.
- `strategy_state.broker_leg_evidence` is non-null.
- Existing active broker exposure is represented in `spread_reconciliation_preview`.

## Restore

If validation fails, stop the engine before restoring:

```bash
docker compose --env-file .env -f deploy/alpaca/compose.yml --profile engine stop alpaca-options
```

Restore the operational schema from a custom dump:

```bash
docker compose --env-file .env -f deploy/alpaca/compose.yml exec -T postgres \
  sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c "DROP SCHEMA IF EXISTS trading_ops CASCADE"'
docker compose --env-file .env -f deploy/alpaca/compose.yml exec -T postgres \
  sh -lc 'pg_restore -U "$POSTGRES_USER" -d "$POSTGRES_DB"' \
  < backups/alpaca-operational/trading_ops-YYYYMMDDTHHMMSSZ.dump
```

Then recreate the engine through `.env` once the schema is healthy.
