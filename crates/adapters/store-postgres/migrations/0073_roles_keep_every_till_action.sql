-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Every role that exists keeps every till action it has today
-- ([ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md), Rollout).
--
-- ADR-0158 gave a name to seven acts that needed none before (#560), and the edge checks each one
-- (decision 2): seating, clearing and releasing a table (`sales.table.manage`), adding a line
-- (`sales.line.add`), firing (`sales.line.fire`), bumping a ticket (`sales.ticket.bump`), opening a
-- bill (`billing.bill.open`), splitting and merging (`billing.bill.split`) and taking a payment
-- (`billing.payment.take`). A role authored before then lists none of them, so the day its store
-- decides with each person's own set, its holders would lose acts nobody ever meant to take from
-- them. This grants all seven to every role that exists, so no role loses an action merely because
-- the action gained a name. What a role grants after this is the owner's to change in the console.
--
-- **Once, not on every boot.** Every file here runs again at each start-up (ADR-0017), and a grant
-- that ran again would hand back a permission an owner had since removed on purpose. So the grant
-- is gated on a marker: the first run inserts `0073_roles_keep_every_till_action` into
-- `data_migrations` and grants in the same statement; every later run inserts nothing, and the
-- `UPDATE` then matches no row. One statement, so the marker and the grant commit together or not
-- at all.
--
-- `data_migrations` is the cloud's own bookkeeping, not tenant data: no RLS and no grant to
-- `app_tenant`, so only the trusted pool owner that runs migrations reads or writes it. A later
-- one-time data change takes its own row under its own file's name.
--
-- Archived roles are granted too. An archived role contributes nothing to anyone (ADR-0158
-- decision 7), and granting it as well means unarchiving one restores what it had. Each role's list
-- stays sorted and free of duplicates, as the console writes it. Forward-only and additive: nothing
-- is dropped, renamed or narrowed.

CREATE TABLE IF NOT EXISTS data_migrations (
    name         text        PRIMARY KEY,
    applied_time timestamptz NOT NULL DEFAULT now()
);

WITH first_run AS (
    INSERT INTO data_migrations (name)
    VALUES ('0073_roles_keep_every_till_action')
    ON CONFLICT (name) DO NOTHING
    RETURNING name
)
UPDATE role_templates AS role_row
   SET permissions = (
           SELECT coalesce(jsonb_agg(DISTINCT granted.id ORDER BY granted.id), '[]'::jsonb)
             FROM (
                   SELECT jsonb_array_elements_text(role_row.permissions) AS id
                   UNION
                   SELECT unnest(ARRAY[
                       'sales.table.manage',
                       'sales.line.add',
                       'sales.line.fire',
                       'sales.ticket.bump',
                       'billing.bill.open',
                       'billing.bill.split',
                       'billing.payment.take'
                   ]) AS id
             ) AS granted
       ),
       updated_at = now()
 WHERE EXISTS (SELECT 1 FROM first_run);
