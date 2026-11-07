-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Which stores hold a personal custom value's subject, and whether each has confirmed masking it
-- ([ADR-0206](../../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)
-- decision 4). Folded by the same projector as `custom_values`, behind the same cursor: a row from
-- each `data.custom_field.set` or `data.custom_record.changed` that names a `subject_id`, and
-- `masked_time` from the store's `data.subject.masked`.
--
-- An id is all the cloud holds: the personal values stay on the store. The erase route reads this
-- table to find the stores an erasure goes to, the console reads it to show an erasure pending, and
-- the projector reads it to take a confirmed id off a store's `erasures` node. A subject is read one
-- at a time, by id, never listed.
--
-- Tenant-scoped: RLS on `app.tenant_id`. The projector writes through the owner pool;
-- `app_tenant` reads. Forward-only and additive, applied idempotently on every boot (ADR-0017).

CREATE TABLE IF NOT EXISTS custom_subjects (
    tenant_id   text        NOT NULL,
    store_id    text        NOT NULL,
    subject_id  text        NOT NULL,
    first_time  timestamptz NOT NULL,
    masked_time timestamptz,
    PRIMARY KEY (tenant_id, store_id, subject_id)
);
CREATE INDEX IF NOT EXISTS custom_subjects_by_subject
    ON custom_subjects (tenant_id, subject_id);

ALTER TABLE custom_subjects ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS custom_subjects_tenant_isolation ON custom_subjects;
CREATE POLICY custom_subjects_tenant_isolation ON custom_subjects
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT ON custom_subjects TO app_tenant;
