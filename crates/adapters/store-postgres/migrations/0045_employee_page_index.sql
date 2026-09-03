-- 0045 · the index the paged employee read needs (ADR-0098, roadmap v3 B3-2 tail).
--
-- `employees` has carried `employees_by_tenant (tenant_id)` since 0023, which serves the tenant
-- filter and nothing else: the read's `ORDER BY created_at DESC` finished with a sort above the
-- scan, and `LIMIT` then truncated a completed sort of the tenant's whole roster. This index
-- carries the order too, so a page stops the scan instead — and makes the `count(*) OVER()` walk
-- index-only.
--
-- The `id` column is not decoration. PostgreSQL's `now()` is **transaction** time, so every row one
-- transaction inserts carries the identical `created_at`: importing a hundred staff from a CSV
-- gives a hundred rows one instant. `created_at DESC` alone therefore does not order them, and a
-- `LIMIT`/`OFFSET` window over a non-total order can return a row on two pages or on neither
-- (ADR-0098 decision 9). Ending on the primary key makes the order total.
--
-- Additive and idempotent (ADR-0017): a new index, no column or constraint touched, `IF NOT EXISTS`
-- so a re-run is a no-op. `employees_by_tenant` stays — it is narrower and still the right index for
-- a plain existence check, and dropping an index is not an additive change.
CREATE INDEX IF NOT EXISTS employees_by_tenant_newest
    ON employees (tenant_id, created_at DESC, id DESC);
