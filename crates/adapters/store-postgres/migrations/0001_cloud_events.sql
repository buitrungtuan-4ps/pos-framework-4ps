-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The cloud event log (P7). Every store's every event lands here.
--
--  * Range-partitioned monthly on business_date (ADR-0022): retention drops a whole month, and a
--    store's trading day lands in one partition because business_date is the store's day, not the
--    server's clock.
--  * Idempotent by event_id (ADR-0026 § EventStore): the primary key includes the partition key,
--    as a partitioned table requires, and a replayed event carries the same business_date, so
--    (business_date, event_id) uniqueness is event_id uniqueness in practice.
--  * Tenant isolation is RLS on tenant_id (ADR-0008/0016): a column and a policy, orthogonal to the
--    partition key, so isolation holds identically on every partition and a query that forgets the
--    tenant sees nothing.
--
-- The envelope is `json`, not `jsonb`. The log is immutable and append-only, and this adapter only
-- ever reads a whole envelope back — every field it filters on (business_date, event_id, tenant_id,
-- store_id) is a promoted column with its own index, so jsonb's operators and GIN indexes buy
-- nothing here. What `json` buys is that it stores the exact bytes it was handed: the EventStore
-- contract (ADR-0026 § idempotency) requires a colliding event to read back byte-for-byte identical
-- to the first writer's, and jsonb — which reorders keys and reformats whitespace — cannot honour
-- that. Rollup tables (P7, derived and mutable) are free to use jsonb.

CREATE TABLE IF NOT EXISTS events (
    business_date date        NOT NULL,
    event_id      text        NOT NULL,
    tenant_id     text        NOT NULL,
    store_id      text        NOT NULL,
    envelope      json        NOT NULL,
    ingested_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (business_date, event_id)
) PARTITION BY RANGE (business_date);

-- Indexes every partition inherits. store+event answers `contains` and per-store reads; the tenant
-- composite answers the rollup and webhook queries.
CREATE INDEX IF NOT EXISTS events_store_event ON events (store_id, event_id);
CREATE INDEX IF NOT EXISTS events_tenant_store_date ON events (tenant_id, store_id, business_date);

-- The safety-net partition: a row whose month has no explicit partition lands here and is alarmed on
-- rather than dropped (ADR-0022). pos_cloud's scheduler creates each month's partition ahead of need.
CREATE TABLE IF NOT EXISTS events_default PARTITION OF events DEFAULT;

-- Creates the monthly partition covering `month` (any date within it), idempotently. Called ahead of
-- need by the cloud; here so the migration ships the mechanism, not only the safety net.
CREATE OR REPLACE FUNCTION create_events_partition(month date) RETURNS void AS $fn$
DECLARE
    start_of_month date := date_trunc('month', month)::date;
    next_month     date := (date_trunc('month', month) + interval '1 month')::date;
    partition_name text := format('events_p_%s', to_char(start_of_month, 'YYYY_MM'));
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_class WHERE relname = partition_name) THEN
        EXECUTE format(
            'CREATE TABLE %I PARTITION OF events FOR VALUES FROM (%L) TO (%L)',
            partition_name, start_of_month, next_month
        );
    END IF;
END;
$fn$ LANGUAGE plpgsql;

-- Row-level security. A row is visible only to the session's tenant; a session that has not set
-- app.tenant_id sees nothing (default-deny — current_setting(..., true) is NULL, and NULL = anything
-- is never true). RLS is not FORCEd, so the owner and a superuser bypass it: the trusted ingest
-- adapter connects as such a role, while the query layer assumes app_tenant and is filtered.
ALTER TABLE events ENABLE ROW LEVEL SECURITY;
-- DROP-then-CREATE rather than a bare CREATE, because `CREATE POLICY` is not idempotent and this
-- migration must be safe to run on every boot (ADR-0017). The drop is a no-op the first time.
DROP POLICY IF EXISTS events_tenant_isolation ON events;
CREATE POLICY events_tenant_isolation ON events
    USING (tenant_id = current_setting('app.tenant_id', true));

-- The per-tenant application role the query layer assumes (ADR-0016). It logs in via the cloud's
-- pooled connection with a scoped password in production; here it needs only the table grants.
DO $do$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'app_tenant') THEN
        CREATE ROLE app_tenant NOLOGIN;
    END IF;
END $do$;
GRANT SELECT, INSERT ON events TO app_tenant;

-- The onward outbox. Present for EventStore port conformance (ADR-0026); the cloud's real outbound is
-- the webhook cursor over the log (P7), so nothing drains this in the cloud today.
CREATE TABLE IF NOT EXISTS event_outbox (
    position bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    store_id text NOT NULL,
    envelope json NOT NULL
);
CREATE INDEX IF NOT EXISTS event_outbox_store ON event_outbox (store_id, position);
