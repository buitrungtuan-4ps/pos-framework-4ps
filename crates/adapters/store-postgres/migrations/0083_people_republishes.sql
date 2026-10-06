-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The stores whose people the cloud publishes again by itself, because a migration changed what
-- their roles grant ([ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)).
--
-- A store's `permissions` node is compiled from its people, roles and assignments when they are
-- published, by **Publish** on People or by a console edit that publishes at once. A migration
-- that changes what roles grant, as 0073, 0074, 0076 and 0079 did, changes no store's node, so
-- until now every store's People had to be published once by hand after one. Instead such a
-- migration queues the stores here, and `pos-cloud` drains the queue at start-up and every few
-- minutes after: it compiles each store's people and publishes them exactly as **Publish** on
-- People does, gives a store whose node is already what its people compile to no new version, and
-- clears the row. A row it cannot publish stays for the next drain. One row is one store:
--
--   * `reason`        — the migration that queued the store, by its file's name, which the audit
--                       trail records as the publish's cause.
--   * `enqueued_time` — when it was queued, to the millisecond. The drain clears a row only while it
--                       still carries the time the drain read, so a store queued again meanwhile is
--                       published again.
--
-- Queuing a store that is queued already moves its reason and time forward: one publish compiles
-- every grant there is by then.
--
-- **Only a store whose people have been published is queued**: a store whose configuration holds a
-- `permissions` roster, a `permissions` object with a `staff` key, wherever a rollback left it. A
-- People publish writes the roster on the Store layer, and a rollback writes the whole restored
-- version onto the Tenant layer and empties the others, so every layer is looked at. A
-- `permissions` object with no `staff` key is the switch alone, `permissions.enforced`, which the
-- settings publish writes on the Tenant layer: the edge reads it as no roster, and so does this. A
-- store nobody published people for is not queued, and the drain never creates a node for it.
-- Queuing too many costs nothing, because the drain decides on the four layers merged, the
-- document the store runs, and gives a store with no roster there no node; queuing too few is a
-- store the grant never reaches.
--
-- This migration queues, once, every such store, which covers the grants of 0073, 0074, 0076 and
-- 0079 for a fleet deployed before it. **Once, not on every boot**, gated on a `data_migrations`
-- marker as those four are: a later boot inserts no marker, so it queues nothing.
--
-- **The convention for a later migration that changes what roles grant**: queue the stores in the
-- same statement as the grant, gated on the same marker, with the file's own name as the reason —
--
--     WITH first_run AS (
--         INSERT INTO data_migrations (name) VALUES ('NNNN_name')
--         ON CONFLICT (name) DO NOTHING RETURNING name
--     ), granted AS (
--         UPDATE role_templates SET ... WHERE EXISTS (SELECT 1 FROM first_run) ... RETURNING 1
--     )
--     INSERT INTO people_republishes (tenant_id, store_id, reason, enqueued_time)
--     SELECT tree.tenant_id, tree.store_id, 'NNNN_name', date_trunc('milliseconds', now())
--       FROM config_trees AS tree
--      WHERE EXISTS (SELECT 1 FROM first_run)
--        AND EXISTS (
--              SELECT 1
--                FROM jsonb_array_elements(
--                       CASE jsonb_typeof(tree.state -> 'layers')
--                            WHEN 'array' THEN tree.state -> 'layers'
--                            ELSE '[]'::jsonb
--                       END
--                     ) AS layer
--               WHERE jsonb_typeof(layer -> 'permissions') = 'object'
--                 AND (layer -> 'permissions') ? 'staff'
--            )
--     ON CONFLICT (tenant_id, store_id) DO UPDATE
--        SET reason = EXCLUDED.reason, enqueued_time = EXCLUDED.enqueued_time;
--
-- The `CASE` hands a state whose `layers` is not an array no layers, so a malformed row queues
-- nothing rather than failing the boot.
--
-- No customer or employee identifier: a tenant, a store and a migration's name. Tenant-scoped like
-- the rest of the config data: RLS on `app.tenant_id`, granted to `app_tenant`, the trusted pool
-- owner that runs migrations and the drain bypassing RLS. Forward-only and additive, applied
-- idempotently on every boot (ADR-0017).

CREATE TABLE IF NOT EXISTS people_republishes (
    tenant_id     text        NOT NULL,
    store_id      text        NOT NULL,
    reason        text        NOT NULL,
    enqueued_time timestamptz NOT NULL,
    PRIMARY KEY (tenant_id, store_id)
);

ALTER TABLE people_republishes ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS people_republishes_tenant_isolation ON people_republishes;
CREATE POLICY people_republishes_tenant_isolation ON people_republishes
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON people_republishes TO app_tenant;

WITH first_run AS (
    INSERT INTO data_migrations (name)
    VALUES ('0083_people_republishes')
    ON CONFLICT (name) DO NOTHING
    RETURNING name
)
INSERT INTO people_republishes (tenant_id, store_id, reason, enqueued_time)
SELECT tree.tenant_id,
       tree.store_id,
       '0083_people_republishes',
       date_trunc('milliseconds', now())
  FROM config_trees AS tree
 WHERE EXISTS (SELECT 1 FROM first_run)
   AND EXISTS (
         SELECT 1
           FROM jsonb_array_elements(
                  CASE jsonb_typeof(tree.state -> 'layers')
                       WHEN 'array' THEN tree.state -> 'layers'
                       ELSE '[]'::jsonb
                  END
                ) AS layer
          WHERE jsonb_typeof(layer -> 'permissions') = 'object'
            AND (layer -> 'permissions') ? 'staff'
       )
ON CONFLICT (tenant_id, store_id) DO UPDATE
   SET reason = EXCLUDED.reason, enqueued_time = EXCLUDED.enqueued_time;
