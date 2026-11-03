-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Every role that exists and runs the store reads a shift's X with its expected cash
-- ([ADR-0187](../../../../docs/adr/0187-a-shift-is-handed-over-at-the-till-and-read-there.md)
-- decision 4).
--
-- `reports.shift.read_x` is new. It asks no PIN, and its default roles are the manager and the owner,
-- so a new tenant's starting roles get it. A role authored before it existed names it nowhere, so
-- this grants it once, **directly**, to every role that grants `admin.config.edit` directly: the
-- permission whose default roles are the same two, the people who run the store. Applied to a new
-- tenant's starting roles the rule picks exactly the manager and the owner, so existing and new
-- tenants agree. It is not PIN-flagged, so no role is given it with approval: a permission held
-- only with approval that no route asks an approver for lets its holder do nothing.
--
-- Nothing else changes. A role's direct list gains the permission only where it grants
-- `admin.config.edit` directly, and is otherwise left byte for byte as it was; the list it writes
-- stays sorted in byte order (`COLLATE "C"`), the order the compiler sorts in, and free of
-- duplicates. The with-approval list is never touched.
--
-- **Once, not on every boot**, gated on a `data_migrations` marker as 0084, 0086 and 0087 are: the
-- first run inserts `0088_roles_can_read_a_shifts_x` and grants in the same statement, and every
-- later run inserts nothing, so a later boot never hands back a permission an owner has since taken
-- away. Archived roles are included, so restoring one restores what it had.
--
-- **In the same statement, it queues** every store whose people have been published in
-- `people_republishes`, with this file's name as the reason, as 0084, 0086 and 0087 do.
--
-- Forward-only and additive: nothing is dropped, renamed or narrowed.

WITH first_run AS (
    INSERT INTO data_migrations (name)
    VALUES ('0088_roles_can_read_a_shifts_x')
    ON CONFLICT (name) DO NOTHING
    RETURNING name
), granted AS (
    UPDATE role_templates AS role_row
       SET permissions = (
               SELECT jsonb_agg(DISTINCT granted.id COLLATE "C" ORDER BY granted.id COLLATE "C")
                 FROM (
                       SELECT jsonb_array_elements_text(role_row.permissions) AS id
                       UNION
                       SELECT 'reports.shift.read_x' AS id
                 ) AS granted
           ),
           updated_at = now()
     WHERE EXISTS (SELECT 1 FROM first_run)
       AND role_row.permissions ? 'admin.config.edit'
    RETURNING 1
)
INSERT INTO people_republishes (tenant_id, store_id, reason, enqueued_time)
SELECT tree.tenant_id,
       tree.store_id,
       '0088_roles_can_read_a_shifts_x',
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
