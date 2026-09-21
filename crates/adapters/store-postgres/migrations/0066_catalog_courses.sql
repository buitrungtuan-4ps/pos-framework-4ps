-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0066 — the course entity ([ADR-0130](../../../docs/adr/0130-a-course-is-something-the-catalog-names.md)).
--
-- A course was built end to end and the thing itself did not exist. `CourseId` is a wire identifier
-- whose own doc says what it is for; `sales.order_line.added` carries one; `POST
-- /api/tables/{id}/lines` accepts one; `pos_proto::floor::RoutingRule` matches on one and
-- `route_station` honours it; `Capability::Courses` gates a fire-by-course and `decide_line` refuses
-- it when the capability is off, with a test. Nothing anywhere created, named, ordered, listed or
-- published a course, so **every `course_id` in the tree was a foreign key to a table that was not
-- there** — sharpest in the admin API, where a routing rule keyed on a course is accepted after
-- checking only that it does not also name an item, published to a store, honoured there, and
-- matches nothing for ever.
--
-- **`sort` is the point, not decoration.** "Starter before main before dessert" is the entire
-- meaning of the grouping; a set of names with no order is a taxonomy, not a service sequence. Ties
-- break by id in the compiler, as everywhere else, so a re-compile of unchanged authoring stays
-- byte-identical.
--
-- A store that serves dessert before the main course for a tasting menu authors it that way:
-- nothing here privileges a particular sequence, and nothing validates the names against a list.
--
-- Tenant-scoped exactly like the rest of the catalog: RLS on `app.tenant_id`, a grant to
-- `app_tenant`, the trusted pool owner bypassing RLS. Forward-only and additive, applied
-- idempotently on every boot (ADR-0017).

CREATE TABLE IF NOT EXISTS catalog_courses (
    course_id  text        PRIMARY KEY,
    tenant_id  text        NOT NULL,
    name       text        NOT NULL,
    -- Where this course falls in the service sequence. Not unique: two courses a store considers
    -- interchangeable may share a position, and forcing an operator to renumber the whole menu to
    -- insert one between two others would be a worse rule than letting the id break the tie.
    sort       integer     NOT NULL DEFAULT 0,
    status     text        NOT NULL DEFAULT 'active',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT catalog_courses_status CHECK (status IN ('active', 'archived'))
);
CREATE INDEX IF NOT EXISTS catalog_courses_by_tenant
    ON catalog_courses (tenant_id);

ALTER TABLE catalog_courses ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS catalog_courses_tenant_isolation ON catalog_courses;
CREATE POLICY catalog_courses_tenant_isolation ON catalog_courses
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON catalog_courses TO app_tenant;

-- The item declares its course, exactly as it carries its tax class (ADR-0130 decision 3). A pizza
-- is a main; making a server say so on every tap would be a tap per dish for a fact the catalog
-- knows. NULL is "on no course", which is every item in every store today and stays correct.
--
-- No foreign key, deliberately, and for the reason the rest of this schema has none: the catalog's
-- references are checked at the write by the compiler and the routes, which can say *which* field
-- was wrong and why, where a constraint violation can only say that one was.
ALTER TABLE catalog_items
    ADD COLUMN IF NOT EXISTS course_id text;
