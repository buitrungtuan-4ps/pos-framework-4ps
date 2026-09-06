// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The print queue an agent claims from (migration 0009,
//! [ADR-0112](../../../../docs/adr/0112-print-agents.md)).
//!
//! Edge-local durable state rather than a port, like the receipt and queue counters and the lease, so
//! it is proven here directly rather than in the shared contract suite. Four properties are the
//! reason the table exists, and each is a way the in-memory version loses a ticket:
//!
//!  * **The cap is per printer.** One jammed kitchen printer must not consume the receipt printer's
//!    budget on the same terminal, and the count must ignore rows that have already expired.
//!  * **One job per printer, but every printer.** ESC/POS is a byte stream and two writers to one
//!    printer interleave garbage — so one at a time there — while a jammed device must not stall its
//!    neighbours.
//!  * **A claim lapses.** An agent that dies holding a job does not hold it forever.
//!  * **An expired job is deleted, never delivered.** A ticket printed an hour late is cooked
//!    against a bill that settled and walks out to a table that left.

// The whole file is test scaffolding; a failed temp dir or runtime is an unrecoverable setup fault.
#![allow(
    clippy::expect_used,
    reason = "test scaffolding: a failed temp dir, runtime, or writer reply is an unrecoverable fault"
)]

use std::future::Future;
use std::path::Path;

use store_sqlite::{PrintEnqueue, QueuedPrintJob, SqliteStore};
use tempfile::TempDir;

/// Drives a future on a fresh current-thread runtime — the executor a real edge binary supplies.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build a current-thread tokio runtime")
        .block_on(future)
}

fn open(path: &Path) -> SqliteStore {
    SqliteStore::open(path).expect("open the store")
}

/// A job for `printer`, owned by `agent`. The document stands in for the rendered blocks; nothing in
/// this table parses it, which is the point — the agent decides nothing about the bytes.
fn job(id: &str, printer: &str, agent: &str) -> QueuedPrintJob {
    QueuedPrintJob {
        job_id: id.to_owned(),
        store_id: "store-1".to_owned(),
        printer_device_id: printer.to_owned(),
        agent_device_id: agent.to_owned(),
        document: format!("{{\"blocks\":[\"{id}\"]}}"),
    }
}

/// A minute in milliseconds, so the instants below read as the service they model.
const MINUTE: i64 = 60_000;

#[test]
fn the_cap_is_per_printer_and_counts_only_unexpired_jobs() {
    let dir = TempDir::new().expect("temp dir");
    block_on(async {
        let store = open(&dir.path().join("store.sqlite"));
        let now = 1_777_000_000_000;
        let ttl = 10 * MINUTE;

        // A cap of 2 rather than the shipped 200: the boundary is the behaviour under test, and
        // writing 200 rows to reach it would prove the same thing more slowly.
        for id in ["a1", "a2"] {
            assert_eq!(
                store
                    .enqueue_print_job(job(id, "printer-hot", "agent-1"), now, now + ttl, 2)
                    .await
                    .expect("the enqueue reached the database"),
                PrintEnqueue::Queued,
            );
        }
        // The third is refused, and nothing is dropped to make room: adding a job to a printer that
        // has not consumed its allowance is promising paper that is not coming.
        assert_eq!(
            store
                .enqueue_print_job(job("a3", "printer-hot", "agent-1"), now, now + ttl, 2)
                .await
                .expect("the enqueue reached the database"),
            PrintEnqueue::QueueFull,
        );

        // The receipt printer on the SAME agent is unaffected — the budget is the printer's.
        assert_eq!(
            store
                .enqueue_print_job(job("b1", "printer-till", "agent-1"), now, now + ttl, 2)
                .await
                .expect("the enqueue reached the database"),
            PrintEnqueue::Queued,
        );

        // And once the first two have aged past their TTL the printer is clear again, without
        // anything having swept them: the count itself ignores expired rows, so a jam that outlasts
        // the TTL does not leave a printer permanently refusing.
        let later = now + ttl + 1;
        assert_eq!(
            store
                .enqueue_print_job(job("a4", "printer-hot", "agent-1"), later, later + ttl, 2)
                .await
                .expect("the enqueue reached the database"),
            PrintEnqueue::Queued,
        );
    });
}

#[test]
fn a_redelivered_enqueue_is_the_same_ticket_and_not_a_second_one() {
    let dir = TempDir::new().expect("temp dir");
    block_on(async {
        let store = open(&dir.path().join("store.sqlite"));
        let now = 1_777_000_000_000;
        assert_eq!(
            store
                .enqueue_print_job(job("same", "printer-till", "agent-1"), now, now + MINUTE, 8)
                .await
                .expect("the enqueue reached the database"),
            PrintEnqueue::Queued,
        );
        // `job_id` is the idempotency key everywhere else in the print path, and the PRIMARY KEY is
        // what makes that true here rather than a check somebody remembered to write.
        assert_eq!(
            store
                .enqueue_print_job(job("same", "printer-till", "agent-1"), now, now + MINUTE, 8)
                .await
                .expect("the enqueue reached the database"),
            PrintEnqueue::AlreadyQueued,
        );
        let claimed = store
            .claim_print_jobs("agent-1".to_owned(), now, now + MINUTE)
            .await
            .expect("the claim reached the database");
        assert_eq!(claimed.len(), 1, "one row, not two: {claimed:?}");
    });
}

#[test]
fn a_claim_takes_the_oldest_job_for_each_printer_the_agent_owns() {
    let dir = TempDir::new().expect("temp dir");
    block_on(async {
        let store = open(&dir.path().join("store.sqlite"));
        let now = 1_777_000_000_000;
        let ttl = 10 * MINUTE;
        // Two printers on one agent, two jobs each, queued oldest-first.
        for (index, (id, printer)) in [
            ("hot-1", "printer-hot"),
            ("till-1", "printer-till"),
            ("hot-2", "printer-hot"),
            ("till-2", "printer-till"),
        ]
        .into_iter()
        .enumerate()
        {
            let at = now + i64::try_from(index).expect("a small index") * 1_000;
            store
                .enqueue_print_job(job(id, printer, "agent-1"), at, at + ttl, 8)
                .await
                .expect("the enqueue reached the database");
        }
        // A third printer on a DIFFERENT agent, which this claim must not touch.
        store
            .enqueue_print_job(job("other-1", "printer-bar", "agent-2"), now, now + ttl, 8)
            .await
            .expect("the enqueue reached the database");

        let mut claimed = store
            .claim_print_jobs("agent-1".to_owned(), now + 10_000, now + 10_000 + 30_000)
            .await
            .expect("the claim reached the database");
        claimed.sort_by(|left, right| left.job_id.cmp(&right.job_id));
        let ids: Vec<&str> = claimed.iter().map(|job| job.job_id.as_str()).collect();
        assert_eq!(
            ids,
            ["hot-1", "till-1"],
            "the oldest per printer, one each, and nothing belonging to another agent"
        );

        // A second claim while the first lease holds returns nothing: one job per printer at a time,
        // because two concurrent writers to one printer interleave garbage.
        let again = store
            .claim_print_jobs("agent-1".to_owned(), now + 11_000, now + 11_000 + 30_000)
            .await
            .expect("the claim reached the database");
        assert!(again.is_empty(), "the lease still holds: {again:?}");
    });
}

#[test]
fn a_lapsed_claim_returns_the_job_and_an_acknowledged_one_is_gone() {
    let dir = TempDir::new().expect("temp dir");
    block_on(async {
        let store = open(&dir.path().join("store.sqlite"));
        let now = 1_777_000_000_000;
        let ttl = 10 * MINUTE;
        store
            .enqueue_print_job(job("only", "printer-till", "agent-1"), now, now + ttl, 8)
            .await
            .expect("the enqueue reached the database");

        let lease = 30_000;
        let claimed = store
            .claim_print_jobs("agent-1".to_owned(), now, now + lease)
            .await
            .expect("the claim reached the database");
        assert_eq!(claimed.len(), 1);
        assert_eq!(
            claimed.first().expect("one claimed job").claim_expires_at,
            now + lease
        );

        // The agent died holding it. Past the lease the job is claimable again — by the same agent
        // after a restart, which is the case this is for.
        let after = now + lease + 1;
        let reclaimed = store
            .claim_print_jobs("agent-1".to_owned(), after, after + lease)
            .await
            .expect("the claim reached the database");
        assert_eq!(
            reclaimed.len(),
            1,
            "a claim nobody acknowledged does not hold the job forever"
        );

        // Acknowledged, the row is gone: nothing to redeliver and nothing left holding PII.
        assert!(
            store
                .acknowledge_print_job("only".to_owned())
                .await
                .expect("the acknowledgement reached the database")
        );
        // A second acknowledgement is `false` and not an error — it happens when a reply is lost on
        // the wire and the agent asks again, and the queue no longer holding it is what it wanted.
        assert!(
            !store
                .acknowledge_print_job("only".to_owned())
                .await
                .expect("the acknowledgement reached the database")
        );
        let empty = store
            .claim_print_jobs("agent-1".to_owned(), after, after + lease)
            .await
            .expect("the claim reached the database");
        assert!(empty.is_empty());
    });
}

#[test]
fn an_expired_job_is_deleted_and_never_claimed() {
    let dir = TempDir::new().expect("temp dir");
    block_on(async {
        let store = open(&dir.path().join("store.sqlite"));
        let now = 1_777_000_000_000;
        let ttl = 10 * MINUTE;
        store
            .enqueue_print_job(job("stale", "printer-till", "agent-1"), now, now + ttl, 8)
            .await
            .expect("the enqueue reached the database");
        store
            .enqueue_print_job(
                job("fresh", "printer-till", "agent-1"),
                now + ttl,
                now + ttl + ttl,
                8,
            )
            .await
            .expect("the enqueue reached the database");

        // Past its TTL the stale job is not claimable, even before anything sweeps it. The claim's
        // own predicate is what guarantees a late ticket never prints; the sweep only reclaims disk.
        let after = now + ttl + 1;
        let claimed = store
            .claim_print_jobs("agent-1".to_owned(), after, after + 30_000)
            .await
            .expect("the claim reached the database");
        assert_eq!(claimed.len(), 1);
        assert_eq!(
            claimed.first().expect("one claimed job").job_id,
            "fresh",
            "the expired job is not delivered"
        );

        assert_eq!(
            store
                .expire_print_jobs(after)
                .await
                .expect("the sweep reached the database"),
            1,
            "the sweep deletes the expired job and leaves the live one"
        );
        // And the surviving row is still the fresh one, still claimable once its lease lapses.
        let survivors = store
            .claim_print_jobs("agent-1".to_owned(), after + 60_000, after + 90_000)
            .await
            .expect("the claim reached the database");
        assert_eq!(survivors.len(), 1);
        assert_eq!(survivors.first().expect("one survivor").job_id, "fresh");
    });
}

#[test]
fn the_queue_survives_a_restart() {
    let dir = TempDir::new().expect("temp dir");
    let now = 1_777_000_000_000;
    let ttl = 10 * MINUTE;
    block_on(async {
        let store = open(&dir.path().join("store.sqlite"));
        store
            .enqueue_print_job(job("durable", "printer-till", "agent-1"), now, now + ttl, 8)
            .await
            .expect("the enqueue reached the database");
    });
    // The whole reason the queue is a table: an install deliberately restarts the edge (ADR-0055),
    // and a queue in process memory loses every job it holds to that.
    block_on(async {
        let store = open(&dir.path().join("store.sqlite"));
        let claimed = store
            .claim_print_jobs("agent-1".to_owned(), now + 1_000, now + 31_000)
            .await
            .expect("the claim reached the database");
        assert_eq!(claimed.len(), 1);
        let only = claimed.first().expect("one claimed job");
        assert_eq!(only.job_id, "durable");
        assert_eq!(only.printer_device_id, "printer-till");
    });
}
