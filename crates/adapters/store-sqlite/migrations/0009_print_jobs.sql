-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0009 — the print queue a print agent claims from (ADR-0112).
--
-- A printer may name the device that owns its transport. That device claims rendered bytes from
-- here, writes them, and says the job id back. The queue is durable because all three of the ways it
-- fails are ordinary: a tablet sleeps, a terminal reboots mid-service, a USB cable is knocked out. A
-- queue in the agent's memory loses jobs to all three, and one in the edge's memory loses them to a
-- restart.
--
-- IT IS A SIDE RECORD, NOT AN EVENT. `AGENTS.md` §2 forbids PII in an event payload, and `document`
-- holds a RENDERED RECEIPT — which may carry a buyer's name and tax code (ADR-0107). So this is the
-- same category as `0004_intake_ledger.sql`: a durable side table the event log does not know about,
-- never logged, never published, deleted on expiry or on acknowledgement. Nothing here is replayed
-- and nothing here reaches the cloud.
--
-- Additive-only (ADR-0017): immutable once merged. A change is a new numbered file.

-- One row per job.
--
-- `job_id` is the PRIMARY KEY because it is already the idempotency key everywhere else: it is what
-- `Printers::print_receipt` takes, what `printer-escpos` dedupes on, and what the agent acknowledges
-- with. Making it the key means a redelivered enqueue cannot become a second ticket at the schema
-- level, not merely by a check somebody remembered to write.
--
-- `document` is the JSON `PrintJob` — the finished blocks the edge rendered. The agent decides
-- nothing about it; its whole contract is open this address, write these bytes, report this id.
--
-- The three instants are Unix milliseconds, and each answers a different question:
--   `queued_at`        — ordering. The oldest unexpired job for a printer is the next one to print.
--   `expires_at`       — `queued_at + JOB_TTL`. A ticket printed an hour late is worse than one that
--                        visibly failed: the late one is cooked against a bill that settled and walks
--                        out to a table that left. So an expired job is deleted, never delivered.
--   `claim_expires_at` — NULL while the job is unclaimed. Set when an agent claims it, so an agent
--                        that dies holding a job does not hold it forever; past that instant the job
--                        is claimable again.
--
-- The TTL and the lease length are NOT in this file. They are constants in one edge module, so
-- changing one is a release rather than a schema change, and the caller writes the computed instant.
CREATE TABLE print_jobs (
    job_id            TEXT NOT NULL,
    store_id          TEXT NOT NULL,
    printer_device_id TEXT NOT NULL,
    agent_device_id   TEXT NOT NULL,
    document          TEXT NOT NULL,
    queued_at         INTEGER NOT NULL,
    expires_at        INTEGER NOT NULL,
    claim_expires_at  INTEGER,
    PRIMARY KEY (job_id)
) WITHOUT ROWID;

-- The claim: an agent asks for the oldest unexpired, unclaimed job per printer it owns.
CREATE INDEX print_jobs_claimable ON print_jobs (agent_device_id, printer_device_id, queued_at);

-- The cap: how many unexpired jobs this printer already holds. Counted PER PRINTER and not per
-- agent, so one jammed kitchen printer cannot consume the receipt printer's budget on the same
-- terminal.
CREATE INDEX print_jobs_per_printer ON print_jobs (printer_device_id, expires_at);
