-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Every role that exists can void a bill that was paid on the terms it voids a bill nobody paid
-- ([ADR-0184](../../../../docs/adr/0184-a-settled-bill-is-voided-whole-not-refunded.md) decision 4,
-- [ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
-- decision 4).
--
-- `billing.bill.void_settled` is new: voiding a settled bill gives back money already taken, which
-- is not the same act as voiding a bill nobody paid, so it is a permission of its own. It is
-- PIN-flagged, and its default roles are `billing.bill.void`'s, so a new tenant's starting roles get
-- it on those terms. A role authored before it existed names it in neither list, so this grants it
-- once, by 0084's rule:
--
-- * **directly** to every role that grants `billing.bill.void` directly, whose holders approve a
--   void today and so approve a void after payment too;
-- * **with approval** to every other role, so its holders can void a paid bill with the code and
--   PIN of somebody who holds it directly, as any PIN-flagged act at a store goes.
--
-- Nothing else in either list changes. A role's direct list gains the permission only where it
-- grants `billing.bill.void` directly, and is otherwise left byte for byte as it was; its
-- with-approval list gains it only where the direct list does not. Each list it writes stays sorted
-- in byte order (`COLLATE "C"`), the order the compiler sorts in, and free of duplicates, and no
-- role names the permission in both.
--
-- **Once, not on every boot**, gated on a `data_migrations` marker exactly as 0073, 0074, 0076,
-- 0079, 0084 and 0086 are: the first run inserts `0087_roles_can_void_a_paid_bill` and grants in
-- the same statement, and every later run inserts nothing, so the `UPDATE` matches no row and a
-- later boot never hands back a permission an owner has since taken away. Archived roles are
-- included, as in those six, so restoring one restores what it had.
--
-- **In the same statement, it queues** every store whose people have been published in
-- `people_republishes`, with this file's name as the reason, as 0084 and 0086 do: a store whose
-- configuration holds a `permissions` roster, a `permissions` object with a `staff` key, on any
-- layer. The `CASE` hands a state whose `layers` is not an array no layers, so a malformed row
-- queues nothing rather than failing the boot.
--
-- Forward-only and additive: nothing is dropped, renamed or narrowed.

WITH first_run AS (
    INSERT INTO data_migrations (name)
    VALUES ('0087_roles_can_void_a_paid_bill')
    ON CONFLICT (name) DO NOTHING
    RETURNING name
), granted AS (
    UPDATE role_templates AS role_row
       SET permissions = CASE
               WHEN role_row.permissions ? 'billing.bill.void' THEN (
                   SELECT jsonb_agg(DISTINCT granted.id COLLATE "C" ORDER BY granted.id COLLATE "C")
                     FROM (
                           SELECT jsonb_array_elements_text(role_row.permissions) AS id
                           UNION
                           SELECT 'billing.bill.void_settled' AS id
                     ) AS granted
               )
               ELSE role_row.permissions
           END,
           permissions_with_approval = CASE
               WHEN role_row.permissions ? 'billing.bill.void'
                   THEN role_row.permissions_with_approval
               ELSE (
                   SELECT jsonb_agg(DISTINCT granted.id COLLATE "C" ORDER BY granted.id COLLATE "C")
                     FROM (
                           SELECT jsonb_array_elements_text(role_row.permissions_with_approval) AS id
                           UNION
                           SELECT 'billing.bill.void_settled' AS id
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
       '0087_roles_can_void_a_paid_bill',
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
