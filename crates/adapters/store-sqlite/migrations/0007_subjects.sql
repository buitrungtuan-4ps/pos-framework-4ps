-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0007 — the store-local subject store (ADR-0107, the twentieth port).
--
-- Where personal data lives so the immutable event log never has to. `pos_proto::pii` makes a name
-- in an event payload a compile error and `docs/pos-spec.md` §15 says why: the log cannot be edited,
-- so anything personal inside it could never be erased. Events therefore carry a subject_id and the
-- person's details sit here, in a row that CAN be scrubbed.
--
-- Written IN THE SAME TRANSACTION as the events that reference the subject (ADR-0107) — a settle
-- that committed without its buyer would print a compliant invoice with no record of who it was
-- for, and a buyer that committed without its settle would hold a person's tax code for a sale that
-- never happened.
--
-- `fields` is the JSON `{name, tax_code, address, …}` document. `masked_at` is NULL while the row
-- still holds personal data and stamped once the retention sweep has replaced every value with
-- [REDACTED]; the sweep keeps subject_id and collected_at, because "held from then until then" is
-- an audit trail rather than personal data. The index is what the sweep runs on: unmasked rows for
-- one store, oldest first.
--
-- Additive-only (ADR-0017): immutable once merged. A change is a new numbered file.

CREATE TABLE subjects (
    store_id     TEXT NOT NULL,
    subject_id   TEXT NOT NULL,
    collected_at INTEGER NOT NULL,
    fields       TEXT NOT NULL,
    masked_at    INTEGER,
    PRIMARY KEY (store_id, subject_id)
) WITHOUT ROWID;

CREATE INDEX subjects_due ON subjects (store_id, collected_at) WHERE masked_at IS NULL;
