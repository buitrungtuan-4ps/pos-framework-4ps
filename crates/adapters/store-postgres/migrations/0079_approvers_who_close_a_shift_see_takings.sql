-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Every approver who closes a shift can see the day's takings
-- ([ADR-0160](../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
-- decision 2, [ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)).
--
-- `reports.takings.view` is new: the Today screen's tile of what the store has taken today, the sum
-- of the day's settled bills and how many there were, and never anyone's own. It is not PIN-flagged,
-- and its default roles are the owner, the manager and the supervisor, so a new tenant's starting
-- roles get it there and nowhere else. Takings are confidential, so a role that existed before must
-- come out where a new tenant's would, and no wider. This grants it once, **directly**, to every role
-- that grants `cash.shift.close` directly **and** grants at least one PIN-flagged permission
-- directly: an approver who closes shifts.
--
-- That is the rule a new tenant's roles follow by construction. A starting role grants a PIN-flagged
-- permission directly only where the catalogue names it a default role, and no PIN-flagged
-- permission names the cashier, the server or the cook, so the approvers who close a shift there are
-- exactly the supervisor, the manager and the owner. A role that existed before got its PIN-flagged
-- permissions with approval from 0074 wherever it did not grant them directly, so a cashier's role
-- holds none directly and does not qualify, while a role an owner made an approver does.
--
-- No other role is given it, and none is given it with approval: the permission is not PIN-flagged,
-- and a figure on a screen is not an act another person approves. A cashier sees the takings only
-- where an owner grants it in the console. The PIN-flagged ids are the catalogue's as of this file,
-- in byte order and once each; a test in pos-cloud (`people::tests`) holds the list to the catalogue,
-- and a PIN-flagged permission added later is named there with the migration that grants it.
--
-- Nothing else changes. A qualifying role's direct list gains `reports.takings.view` and keeps its
-- order (`COLLATE "C"`, the order the compiler sorts in) and its freedom from duplicates, as 0076
-- left it; every other role, and every with-approval list, is left byte for byte as it was.
--
-- **Once, not on every boot**, gated on a `data_migrations` marker exactly as 0073, 0074 and 0076
-- are: the first run inserts `0079_approvers_who_close_a_shift_see_takings` and grants in the same
-- statement, and every later run inserts nothing, so the `UPDATE` matches no row and a later boot
-- never hands back a permission an owner has since taken away. Archived roles are included, as in
-- 0073, 0074 and 0076, so restoring one restores what it had. A store hears of the grant when its
-- `permissions` node is next published. Forward-only and additive: nothing is dropped, renamed or
-- narrowed.

WITH first_run AS (
    INSERT INTO data_migrations (name)
    VALUES ('0079_approvers_who_close_a_shift_see_takings')
    ON CONFLICT (name) DO NOTHING
    RETURNING name
)
UPDATE role_templates AS role_row
   SET permissions = (
           SELECT jsonb_agg(DISTINCT granted.id COLLATE "C" ORDER BY granted.id COLLATE "C")
             FROM (
                   SELECT jsonb_array_elements_text(role_row.permissions) AS id
                   UNION
                   SELECT 'reports.takings.view' AS id
             ) AS granted
       ),
       updated_at = now()
 WHERE EXISTS (SELECT 1 FROM first_run)
   AND role_row.permissions ? 'cash.shift.close'
   AND role_row.permissions ?| ARRAY[
       'billing.bill.void',
       'billing.comp.apply',
       'billing.discount.override_ceiling',
       'billing.fee.waive',
       'billing.price.override',
       'billing.refund.issue',
       'cash.drawer.open_no_sale',
       'sales.line.void_fired'
   ];
