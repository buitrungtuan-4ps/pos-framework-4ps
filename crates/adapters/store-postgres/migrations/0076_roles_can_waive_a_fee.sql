-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Every role that exists can waive a fee on the terms it can exceed the discount ceiling
-- ([ADR-0159](../../../../docs/adr/0159-a-fee-is-configuration.md) decision 5,
-- [ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md) decision 4).
--
-- `billing.fee.waive` is new: waiving a waivable fee on one bill, which forgives money as a
-- discount above the ceiling does. It is PIN-flagged, and its default roles are
-- `billing.discount.override_ceiling`'s, so a new tenant's starting roles get it on those terms.
-- A role authored before it existed names it in neither list, so this grants it once:
--
-- * **directly** to every role that grants `billing.discount.override_ceiling` directly, whose
--   holders approve money off a bill for others today and so approve a waive too;
-- * **with approval** to every other role, so its holders can waive a fee with the code and PIN
--   of somebody who holds it directly, as any PIN-flagged act at a store goes today.
--
-- Nothing else in either list changes. A role's direct list gains `billing.fee.waive` only where
-- it grants the override directly, and is otherwise left byte for byte as it was; its
-- with-approval list gains it only where the direct list does not. Each list it writes stays
-- sorted in byte order (`COLLATE "C"`), the order the compiler sorts in, and free of duplicates,
-- as 0074 left them, and no role names the permission in both.
--
-- **Once, not on every boot**, gated on a `data_migrations` marker exactly as 0073 and 0074 are:
-- the first run inserts `0076_roles_can_waive_a_fee` and grants in the same statement, and every
-- later run inserts nothing, so the `UPDATE` matches no row and a later boot never hands back a
-- permission an owner has since taken away. Archived roles are included, as in 0073 and 0074, so
-- restoring one restores what it had. Forward-only and additive: nothing is dropped, renamed or
-- narrowed.

WITH first_run AS (
    INSERT INTO data_migrations (name)
    VALUES ('0076_roles_can_waive_a_fee')
    ON CONFLICT (name) DO NOTHING
    RETURNING name
)
UPDATE role_templates AS role_row
   SET permissions = CASE
           WHEN role_row.permissions ? 'billing.discount.override_ceiling' THEN (
               SELECT jsonb_agg(DISTINCT granted.id COLLATE "C" ORDER BY granted.id COLLATE "C")
                 FROM (
                       SELECT jsonb_array_elements_text(role_row.permissions) AS id
                       UNION
                       SELECT 'billing.fee.waive' AS id
                 ) AS granted
           )
           ELSE role_row.permissions
       END,
       permissions_with_approval = CASE
           WHEN role_row.permissions ? 'billing.discount.override_ceiling'
               THEN role_row.permissions_with_approval
           ELSE (
               SELECT jsonb_agg(DISTINCT granted.id COLLATE "C" ORDER BY granted.id COLLATE "C")
                 FROM (
                       SELECT jsonb_array_elements_text(role_row.permissions_with_approval) AS id
                       UNION
                       SELECT 'billing.fee.waive' AS id
                 ) AS granted
           )
       END,
       updated_at = now()
 WHERE EXISTS (SELECT 1 FROM first_run);
