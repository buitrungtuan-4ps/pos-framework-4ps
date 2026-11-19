-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Every role that exists can take a kiosk back on the terms it manages the store's devices
-- ([ADR-0224](../../../../docs/adr/0224-self-order-is-a-guest-page-and-a-kiosk.md) decision 4,
-- [ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
-- decision 4).
--
-- `admin.kiosk.exit` is new: leaving a kiosk's self-order screen is a person signing in on it with
-- their PIN, and a permission of its own so a tenant can let a shift lead do it without managing
-- devices. It is PIN-flagged, and its default roles are `admin.device.manage`'s, so a new tenant's
-- starting roles get it on those terms. A role authored before it existed names it in neither
-- list, so this grants it once, by 0087's rule:
--
-- * **directly** to every role that grants `admin.device.manage` directly;
-- * **with approval** to every other role, as every PIN-flagged permission is. Leaving a kiosk
--   needs it held directly, so a with-approval grant lets nobody leave one; it keeps the role's
--   lists in the shape every other PIN-flagged permission has.
--
-- Nothing else in either list changes, each list it writes stays sorted in byte order
-- (`COLLATE "C"`) and free of duplicates, and no role names the permission in both.
--
-- **Once, not on every boot**, gated on a `data_migrations` marker as 0087 is, archived roles
-- included; **in the same statement, it queues** every store whose people have been published in
-- `people_republishes`, with this file's name as the reason, as 0087 does.
--
-- Forward-only and additive: nothing is dropped, renamed or narrowed.

WITH first_run AS (
    INSERT INTO data_migrations (name)
    VALUES ('0104_roles_can_leave_a_kiosk')
    ON CONFLICT (name) DO NOTHING
    RETURNING name
), granted AS (
    UPDATE role_templates AS role_row
       SET permissions = CASE
               WHEN role_row.permissions ? 'admin.device.manage' THEN (
                   SELECT jsonb_agg(DISTINCT granted.id COLLATE "C" ORDER BY granted.id COLLATE "C")
                     FROM (
                           SELECT jsonb_array_elements_text(role_row.permissions) AS id
                           UNION
                           SELECT 'admin.kiosk.exit' AS id
                     ) AS granted
               )
               ELSE role_row.permissions
           END,
           permissions_with_approval = CASE
               WHEN role_row.permissions ? 'admin.device.manage'
                   THEN role_row.permissions_with_approval
               ELSE (
                   SELECT jsonb_agg(DISTINCT granted.id COLLATE "C" ORDER BY granted.id COLLATE "C")
                     FROM (
                           SELECT jsonb_array_elements_text(role_row.permissions_with_approval) AS id
                           UNION
                           SELECT 'admin.kiosk.exit' AS id
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
       '0104_roles_can_leave_a_kiosk',
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
