-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The clock a scheduled publish was timed against (ADR-0125 §2, §26).
--
-- `0064_releases.sql` already says why the instant is per store: "conversion happens on the cloud at
-- schedule time, from each store's published `locale.timezone`, so the operator can be *shown* the
-- forty instants they are committing to." The instant was written down. The zone it came from was
-- not, so the console had nothing to print beside it and drew every row in the *reader's* clock
-- instead. A Ho Chi Minh City operator reviewing a "Monday 04:00 local" release therefore read the
-- Tokyo row as 02:00 — correct as an instant, and not the review ADR-0125 §26 rejected fire-time
-- conversion to get.
--
-- Recorded rather than looked up later, for the reason that record gives about the instant itself:
-- "a timezone change between schedule and fire silently move[s] a publish an operator already
-- approved." Re-reading the store's current zone to *describe* an approved schedule has the same
-- defect one step removed — the row would start reading differently than when it was signed off.
-- The zone belongs to the decision, so it is stored with it.
--
-- Null for two cases that are not the same and do not need to be told apart here: a release given a
-- plain UTC instant, which needs no zone at all (ADR-0125 §24's escape hatch), and a row written
-- before this column existed. Both mean "this console cannot name the clock", and both render the
-- way every timestamp rendered until now.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017). Greenfield — no
-- backfill: the zone of a publish already scheduled was never recorded and cannot be recovered
-- without assuming the store has not moved, which is the assumption this column exists to stop.

ALTER TABLE scheduled_publishes
    ADD COLUMN IF NOT EXISTS resolved_timezone text;
