-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0054 — a retired handover is stored, not only audited (ADR-0110, Program C Phase 1).
--
-- **Why storage, when there is already an audit trail.** ADR-0110 first said `retired` *is* the
-- decision, observable only as an audited `/admin` write. The audit module's own contract says why
-- that cannot hold: `AuditRecorder::record` is best-effort — "a store failure is the recorder's to
-- log and swallow, never the caller's to propagate" (ADR-0069) — because a mutation that succeeded
-- must not fail because its audit write did. A trail permitted to drop an entry cannot be the
-- durable record of a decision. One swallowed failure and a retired machine reads as merely
-- *settled*, which is an invitation to keep paying for its hosting or to trust it again.
--
-- The trail still records *who decided and when, and what the row looked like before*. These columns
-- record *that it was decided*, which is the part a reader must not lose.
--
-- **`retired_by` is the admin's ULID, never their email.** The audit trail already carries the email
-- as it stood at action time, which is where a human-readable actor belongs. Copying it into an
-- operational table the fleet console reads would spread employee personal data into a table nobody
-- classified as holding any, for no gain: the id resolves to a person through `admin_users` when a
-- reader with the standing to ask does. Data minimisation, and it keeps this table's classification
-- honest.
--
-- **These columns describe the current handover, not a history.** One row per store, so they can
-- hold exactly one retirement. A later bump starts a new handover with a new outgoing machine, and
-- the same statement that records the new `superseded_generation` clears these two — otherwise a
-- retirement from three handovers ago would sit on the row describing a machine still in the shop.
-- The history of every retirement is the audit trail's job, and it has it.
--
-- **Two columns rather than one nullable timestamp with a convention.** A reader needs both facts,
-- and a schema in which one can be present without the other is a schema that will eventually hold
-- exactly that. They are written and cleared together, always by the same statement.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017).

-- Unix milliseconds, matching `issued_at` on the same row rather than a `timestamptz`, so every
-- instant this table carries is read the same way. NULL is the normal state: no handover on this
-- store has been retired.
ALTER TABLE store_lease
    ADD COLUMN IF NOT EXISTS retired_at bigint;

-- The deciding admin's ULID. `text` and no `REFERENCES admin_users(id)`: an admin who leaves and is
-- removed must not take the record of their decision with them, and a foreign key would either
-- block that removal or cascade the decision away. The trail holds the same id beside the email.
ALTER TABLE store_lease
    ADD COLUMN IF NOT EXISTS retired_by text;
