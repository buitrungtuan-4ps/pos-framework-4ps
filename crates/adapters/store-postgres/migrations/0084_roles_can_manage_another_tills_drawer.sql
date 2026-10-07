-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Every role that exists can start, count or close another till's drawer on the terms it opens the
-- drawer without a sale ([ADR-0167](../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md)
-- decision 3, [ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
-- decision 4).
--
-- `cash.shift.manage_other_till` is new: starting, counting or closing a drawer that is not the
-- till's own, from any till, one, several or all at once, where `cash.shift.open` and
-- `cash.shift.close` cover a till's own. It is PIN-flagged, and its default roles are
-- `cash.drawer.open_no_sale`'s, so a new tenant's starting roles get it on those terms. A role
-- authored before it existed names it in neither list, so this grants it once:
--
-- * **directly** to every role that grants `cash.drawer.open_no_sale` directly, whose holders
--   approve a drawer opened for others today and so approve another till's drawer too;
-- * **with approval** to every other role, so its holders can manage another till's drawer with
--   the code and PIN of somebody who holds it directly, as any PIN-flagged act at a store goes.
--
-- Nothing else in either list changes. A role's direct list gains the permission only where it
-- grants `cash.drawer.open_no_sale` directly, and is otherwise left byte for byte as it was; its
-- with-approval list gains it only where the direct list does not. Each list it writes stays sorted
-- in byte order (`COLLATE "C"`), the order the compiler sorts in, and free of duplicates, as 0076
-- left them, and no role names the permission in both.
--
-- **Once, not on every boot**, gated on a `data_migrations` marker exactly as 0073, 0074, 0076 and
-- 0079 are: the first run inserts `0084_roles_can_manage_another_tills_drawer` and grants in the
-- same statement, and every later run inserts nothing, so the `UPDATE` matches no row and a later
-- boot never hands back a permission an owner has since taken away. Archived roles are included, as
-- in those four, so restoring one restores what it had.
--
-- **In the same statement, it queues** every store whose people have been published in
-- `people_republishes`, with this file's name as the reason, as 0083's header sets out: a store
-- whose configuration holds a `permissions` roster, a `permissions` object with a `staff` key, on any
-- layer. The cloud publishes each queued store's people again by itself, so the grant reaches its
-- tills without anybody pressing **Publish**. The `CASE` hands a state whose `layers` is not an
-- array no layers, so a malformed row queues nothing rather than failing the boot.
--
-- The permission does nothing at a store until its edge gives each till its own drawer: where one
-- drawer is every till's, nothing asks for it. Forward-only and additive: nothing is dropped,
-- renamed or narrowed.

WITH first_run AS (
    INSERT INTO data_migrations (name)
    VALUES ('0084_roles_can_manage_another_tills_drawer')
    ON CONFLICT (name) DO NOTHING
    RETURNING name
), granted AS (
    UPDATE role_templates AS role_row
       SET permissions = CASE
               WHEN role_row.permissions ? 'cash.drawer.open_no_sale' THEN (
                   SELECT jsonb_agg(DISTINCT granted.id COLLATE "C" ORDER BY granted.id COLLATE "C")
                     FROM (
                           SELECT jsonb_array_elements_text(role_row.permissions) AS id
                           UNION
                           SELECT 'cash.shift.manage_other_till' AS id
                     ) AS granted
               )
               ELSE role_row.permissions
           END,
           permissions_with_approval = CASE
               WHEN role_row.permissions ? 'cash.drawer.open_no_sale'
                   THEN role_row.permissions_with_approval
               ELSE (
                   SELECT jsonb_agg(DISTINCT granted.id COLLATE "C" ORDER BY granted.id COLLATE "C")
                     FROM (
                           SELECT jsonb_array_elements_text(role_row.permissions_with_approval) AS id
                           UNION
                           SELECT 'cash.shift.manage_other_till' AS id
                     ) AS granted
               )
           END,
           updated_at = now()
     WHERE EXISTS (SELECT 1 FROM first_run)
    RETURNING 1
)
INSERT INTO people_republishes (tenant_id, store_id, reason, enqueued_time)
SELECT tree.tenant_id,
       tree.store_id,
       '0084_roles_can_manage_another_tills_drawer',
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
