-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- A role says which permissions it grants with approval
-- ([ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
-- decision 4).
--
-- A role grants each permission either *directly*, which its holder acts on alone, or *with
-- approval*, where another person who holds it directly enters their code and PIN for each act.
-- `permissions` stays the list a role grants directly, exactly as before. The new column is the
-- list it grants with approval: a jsonb array of pos-core permission ids, never one that is also
-- in `permissions`.
--
-- **Every role that exists keeps what its holders can do with somebody's approval.** Today any
-- PIN-flagged act at a store goes through with the PIN of somebody who holds it, whoever started
-- it. So each existing role is given, once, every PIN-flagged permission it does not already grant
-- directly, with approval. A role that grants one directly keeps it directly: its holders go on
-- approving it for others, and once their store enforces each person's own set they act on it
-- alone. `permissions` is not touched, so the list each store decides with today is byte-identical
-- afterwards, and a store that does not enforce each person's own set decides without the new one.
--
-- The PIN-flagged permissions are the `pin_required=true` lines of
-- `docs/snapshots/permissions.txt`. A test in pos-cloud (`people::tests`) holds the literal list
-- below to `Permission::ALL`, so the two cannot drift. Each list is sorted in byte order
-- (`COLLATE "C"`), the order the compiler sorts in whatever the database's collation, and has no
-- duplicates.
--
-- **Once, not on every boot**, gated on a `data_migrations` marker exactly as 0073 is: the first
-- run inserts `0074_role_permissions_with_approval` and grants in the same statement, and every
-- later run inserts nothing, so the `UPDATE` matches no row and a later boot never hands back an
-- approval an owner has since taken away. Archived roles are included, as in 0073, so restoring
-- one restores what it had. Forward-only and additive: nothing is dropped, renamed or narrowed.

ALTER TABLE role_templates
    ADD COLUMN IF NOT EXISTS permissions_with_approval jsonb NOT NULL DEFAULT '[]'::jsonb;

WITH first_run AS (
    INSERT INTO data_migrations (name)
    VALUES ('0074_role_permissions_with_approval')
    ON CONFLICT (name) DO NOTHING
    RETURNING name
)
UPDATE role_templates AS role_row
   SET permissions_with_approval = (
           SELECT coalesce(
                      jsonb_agg(DISTINCT flagged.id COLLATE "C" ORDER BY flagged.id COLLATE "C"),
                      '[]'::jsonb
                  )
             FROM unnest(ARRAY[
                      'billing.bill.void',
                      'billing.comp.apply',
                      'billing.discount.override_ceiling',
                      'billing.price.override',
                      'billing.refund.issue',
                      'cash.drawer.open_no_sale',
                      'sales.line.void_fired'
                  ]) AS flagged(id)
            WHERE NOT role_row.permissions ? flagged.id
       ),
       updated_at = now()
 WHERE EXISTS (SELECT 1 FROM first_run);
