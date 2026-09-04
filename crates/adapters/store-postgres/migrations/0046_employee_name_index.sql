-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- An index covering the roster's *name* order, the second order `GET /admin/employees` now offers
-- (ADR-0098, the People screen's third slice).
--
-- Migration 0045 gave the default order (`created_at DESC, id DESC`) an index. `?sort=name` is a
-- different order and needs its own, or a page is a `LIMIT` over a sort of the tenant's whole
-- roster — the cost 0045 exists to avoid, reintroduced by a query parameter.
--
-- The tiebreaker is here for the reason 0044 gives and 0045 measured: `ORDER BY name` is no more
-- total than `ORDER BY created_at` was. Two employees sharing a name is ordinary rather than
-- exotic — it is one of the reasons the staff *code* exists — and a `LIMIT`/`OFFSET` window over
-- that tie can return a row on two pages or on neither. So the read orders `name, id` and this
-- index carries both.
--
-- Ascending because that is the direction the console asks for; PostgreSQL walks a btree backwards
-- as cheaply as forwards, so the same index serves `?order=desc`.
--
-- No index for `?sort=code`: `employees_code_key (tenant_id, code)` from migration 0023 already
-- covers it, and the `id` tiebreaker the read appends can never fire there, because that index is
-- UNIQUE — one code per tenant means `code` alone already orders the rows totally. Stated so the
-- absence reads as a decision rather than an oversight.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017). No data change.

CREATE INDEX IF NOT EXISTS employees_by_tenant_name
    ON employees (tenant_id, name, id);
