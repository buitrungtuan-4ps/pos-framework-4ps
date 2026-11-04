-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The cloud's projection of custom data
-- ([ADR-0194](../../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md)
-- decision 3): one row per object and field, holding the value it has now, folded from the
-- `data.custom_field.set` and `data.custom_record.changed` events by the projector.
--
-- `object` is the existing object's kind (`DATA_OBJECT_ORDER`, …) and `object_type_id` the object
-- type of a record; exactly one is set. `value` is the event's typed value, which is never personal:
-- free text and every personal value stay on the store, and a row carries at most the `subject_id`
-- of the object's personal record (ADR-0206 decision 3). `custom_value_cursors` holds, per store,
-- the last event folded; a page's rows and its cursor are written in one transaction.
--
-- Tenant-scoped: RLS on `app.tenant_id`. The projector writes through the owner pool;
-- `app_tenant` reads. Forward-only and additive, applied idempotently on every boot (ADR-0017).

CREATE TABLE IF NOT EXISTS custom_values (
    tenant_id          text        NOT NULL,
    store_id           text        NOT NULL,
    object_id          text        NOT NULL,
    custom_field_id    text        NOT NULL,
    object             text,
    object_type_id     text,
    value              jsonb       NOT NULL,
    subject_id         text,
    package_version_id text        NOT NULL,
    update_time        timestamptz NOT NULL,
    PRIMARY KEY (tenant_id, store_id, object_id, custom_field_id)
);
CREATE INDEX IF NOT EXISTS custom_values_by_object_type
    ON custom_values (tenant_id, object_type_id, custom_field_id) WHERE object_type_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS custom_values_by_object
    ON custom_values (tenant_id, object, custom_field_id) WHERE object IS NOT NULL;

ALTER TABLE custom_values ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS custom_values_tenant_isolation ON custom_values;
CREATE POLICY custom_values_tenant_isolation ON custom_values
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT ON custom_values TO app_tenant;

CREATE TABLE IF NOT EXISTS custom_value_cursors (
    tenant_id   text        NOT NULL,
    store_id    text        NOT NULL,
    event_id    text        NOT NULL,
    update_time timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, store_id)
);

ALTER TABLE custom_value_cursors ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS custom_value_cursors_tenant_isolation ON custom_value_cursors;
CREATE POLICY custom_value_cursors_tenant_isolation ON custom_value_cursors
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT ON custom_value_cursors TO app_tenant;
