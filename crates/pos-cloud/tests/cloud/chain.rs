// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The anchor ledger (ADR-0131), recomputing the chain behind an anchor (ADR-0132), and the
//! chain-finding read (`/admin/chain-findings`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pos_cloud::anchor::{
    AnchorConflict, AnchorError, AnchorLedger, ChainWindow, ConflictReport, HeldAnchors,
    StoreAnchor,
};
use pos_cloud::{Cloud, IngestOutcome, http};
use pos_contract_tests::fixtures;
use pos_fakes::FakeStore;
use pos_proto::BusinessDate;
use pos_proto::chain::{ChainHash, ChainLink};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{DeviceId, StoreId, TenantId};
use pos_proto::ulid::Ulid;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeConfigTrees, FakeKeys, FakeRollups, FakeWebhooks, admin_cookie, app_all, clock,
    json_body, provisioned_admin, read_back, store_id, tenant,
};

// --- The anchor ledger (ADR-0131 decision 4) -----------------------------------------------------

/// An anchor ledger in memory, and the one place these cases can see what the cloud decided.
///
/// Deliberately not a spy that only counts calls: what has to be proven is *what the ledger ends up
/// holding*, because a refusal that recorded a conflict and then overwrote the held head anyway
/// would pass a call-counting assertion while destroying the evidence.
#[derive(Debug, Default)]
struct FakeAnchors {
    /// Accepted anchors, keyed by `(store, chain_seq)` — the ledger's idempotency key.
    accepted: Mutex<BTreeMap<(StoreId, u64), StoreAnchor>>,
    conflicts: Mutex<Vec<ConflictReport>>,
    /// When set, every read fails — the cloud must ingest anyway.
    unreadable: bool,
}

impl FakeAnchors {
    fn holding(anchors: &[StoreAnchor]) -> Self {
        let mut accepted = BTreeMap::new();
        for anchor in anchors {
            accepted.insert((anchor.store, anchor.chain_seq), anchor.clone());
        }
        Self {
            accepted: Mutex::new(accepted),
            ..Self::default()
        }
    }

    fn broken() -> Self {
        Self {
            unreadable: true,
            ..Self::default()
        }
    }

    fn heads(&self) -> Vec<(u64, String)> {
        self.accepted
            .lock()
            .expect("the ledger lock")
            .values()
            .map(|anchor| (anchor.chain_seq, anchor.head.as_str().to_owned()))
            .collect()
    }

    fn conflicts(&self) -> Vec<ConflictReport> {
        self.conflicts.lock().expect("the ledger lock").clone()
    }
}

impl AnchorLedger for FakeAnchors {
    fn held_for(
        &self,
        _tenant: TenantId,
        store: StoreId,
        chain_seq: u64,
    ) -> pos_ports::dynamic::BoxFuture<'_, Result<HeldAnchors, AnchorError>> {
        if self.unreadable {
            return Box::pin(async { Err(AnchorError::new("the ledger is down")) });
        }
        let accepted = self.accepted.lock().expect("the ledger lock");
        let held = HeldAnchors {
            head: accepted
                .values()
                .filter(|anchor| anchor.store == store)
                .max_by_key(|anchor| anchor.chain_seq)
                .cloned(),
            at_offered_length: accepted.get(&(store, chain_seq)).cloned(),
        };
        Box::pin(async move { Ok(held) })
    }

    fn accept<'a>(
        &'a self,
        _tenant: TenantId,
        anchor: &'a StoreAnchor,
    ) -> pos_ports::dynamic::BoxFuture<'a, Result<(), AnchorError>> {
        self.accepted
            .lock()
            .expect("the ledger lock")
            .insert((anchor.store, anchor.chain_seq), anchor.clone());
        Box::pin(async { Ok(()) })
    }

    fn record_conflict<'a>(
        &'a self,
        _tenant: TenantId,
        report: &'a ConflictReport,
    ) -> pos_ports::dynamic::BoxFuture<'a, Result<(), AnchorError>> {
        self.conflicts
            .lock()
            .expect("the ledger lock")
            .push(report.clone());
        Box::pin(async { Ok(()) })
    }

    fn list_conflicts(
        &self,
        _tenant: TenantId,
        store: Option<StoreId>,
        limit: u32,
    ) -> pos_ports::dynamic::BoxFuture<'_, Result<Vec<AnchorConflict>, AnchorError>> {
        // Real, not an empty stand-in: a fake that always answered "nothing" would let the console
        // read's tests pass against a route that returns nothing — the exact shape of failure this
        // whole line of work is about.
        let listed: Vec<AnchorConflict> = self
            .conflicts
            .lock()
            .expect("the ledger lock")
            .iter()
            .filter(|report| store.is_none_or(|only| report.store == only))
            .take(limit as usize)
            .enumerate()
            .map(|(index, report)| AnchorConflict {
                conflict_id: format!("conflict-{index}"),
                noticed_at: fixtures::instant(),
                report: report.clone(),
            })
            .collect();
        Box::pin(async move { Ok(listed) })
    }
}

/// An anchor event as a store publishes it at shift close.
fn anchor_event(seed: u32, chain_seq: u64, head: &str) -> EventEnvelope<RawPayload> {
    fixtures::envelope(
        store_id(),
        seed,
        &pos_proto::events::StoreChainAnchored {
            chain_seq,
            chain_head: ChainHash::from_hex(head),
            unchained: 0,
        },
    )
}

/// The anchor as the ledger would have recorded it, for seeding a ledger's prior state.
fn held_anchor(chain_seq: u64, head: &str) -> StoreAnchor {
    StoreAnchor {
        store: store_id(),
        chain_seq,
        head: ChainHash::from_hex(head),
        unchained: 0,
        observed_at: fixtures::instant(),
    }
}

#[tokio::test]
async fn ingesting_an_anchor_records_the_stores_chain_head() {
    let ledger = Arc::new(FakeAnchors::default());
    let cloud = Cloud::new(FakeStore::new()).with_anchor_ledger(ledger.clone());

    cloud
        .ingest(&[anchor_event(1, 42, "ab")])
        .await
        .expect("ingest");

    assert_eq!(
        ledger.heads(),
        vec![(42, "ab".to_owned())],
        "the head a store published is what the cloud now holds"
    );
    assert!(ledger.conflicts().is_empty());
}

#[tokio::test]
async fn a_re_delivered_anchor_changes_nothing() {
    // At-least-once ingest and reconciliation's re-push both guarantee this arrives twice. It must
    // not read as a store contradicting itself.
    let ledger = Arc::new(FakeAnchors::holding(&[held_anchor(42, "ab")]));
    let cloud = Cloud::new(FakeStore::new()).with_anchor_ledger(ledger.clone());

    cloud
        .ingest(&[anchor_event(1, 42, "ab")])
        .await
        .expect("ingest");

    assert_eq!(ledger.heads(), vec![(42, "ab".to_owned())]);
    assert!(
        ledger.conflicts().is_empty(),
        "a delivery guarantee working correctly is not a finding"
    );
}

#[tokio::test]
async fn an_anchor_the_cloud_missed_is_filed_without_disturbing_the_head() {
    // A broker gap lost the anchor at 20 and delivered the one at 42; reconciliation re-pushes 20
    // afterwards. Nothing held contradicts it.
    let ledger = Arc::new(FakeAnchors::holding(&[held_anchor(42, "ab")]));
    let cloud = Cloud::new(FakeStore::new()).with_anchor_ledger(ledger.clone());

    cloud
        .ingest(&[anchor_event(1, 20, "cd")])
        .await
        .expect("ingest");

    assert_eq!(
        ledger.heads(),
        vec![(20, "cd".to_owned()), (42, "ab".to_owned())],
        "the late anchor is kept and the head stays where it was"
    );
    assert!(ledger.conflicts().is_empty());
}

#[tokio::test]
async fn a_second_head_at_one_length_is_refused_and_the_held_one_survives() {
    // The recomputation case ADR-0131 option 2 names, and the one
    // `store-sqlite/tests/chain.rs::editing_and_recomputing_the_whole_chain_is_not_caught_by_the_chain_alone`
    // asserts a bare chain misses: the store rewrote its history, re-derived every link, and now
    // hashes its log of length 42 to something else. The cloud holds the original, and holding it
    // is the whole point — an overwrite would erase the contradiction it exists to show.
    let ledger = Arc::new(FakeAnchors::holding(&[held_anchor(42, "ab")]));
    let cloud = Cloud::new(FakeStore::new()).with_anchor_ledger(ledger.clone());

    cloud
        .ingest(&[anchor_event(1, 42, "ff")])
        .await
        .expect("ingest");

    assert_eq!(
        ledger.heads(),
        vec![(42, "ab".to_owned())],
        "the cloud's record is not overwritten by the one that contradicts it"
    );
    let conflicts = ledger.conflicts();
    assert_eq!(conflicts.len(), 1, "the contradiction is recorded");
    assert_eq!(conflicts[0].chain_seq, 42);
    assert_eq!(conflicts[0].held_head, ChainHash::from_hex("ab"));
    assert_eq!(conflicts[0].offered_head, ChainHash::from_hex("ff"));
    assert_eq!(
        conflicts[0].offered_event_id,
        fixtures::event_id(1),
        "the finding names the event that carried it, so it can be traced to a record"
    );
}

#[tokio::test]
async fn a_refused_anchor_is_still_stored_as_an_event() {
    // The event log is the source of truth, and what a store said is the evidence. Refusing the
    // batch would delete the finding.
    let ledger = Arc::new(FakeAnchors::holding(&[held_anchor(42, "ab")]));
    let cloud = Cloud::new(FakeStore::new()).with_anchor_ledger(ledger.clone());

    let outcome = cloud
        .ingest(&[anchor_event(1, 42, "ff")])
        .await
        .expect("a refused anchor never fails the ingest");

    assert_eq!(
        outcome,
        IngestOutcome {
            appended: 1,
            duplicates: 0
        }
    );
    assert_eq!(read_back(cloud.store()).await.len(), 1);
}

#[tokio::test]
async fn a_ledger_that_cannot_be_read_never_stops_the_log() {
    // An outage that halted ingest is exactly the cover somebody tampering would want, so the
    // failure is logged and the batch stands.
    let ledger = Arc::new(FakeAnchors::broken());
    let cloud = Cloud::new(FakeStore::new()).with_anchor_ledger(ledger.clone());

    let outcome = cloud
        .ingest(&[anchor_event(1, 42, "ab")])
        .await
        .expect("ingest");

    assert_eq!(outcome.appended, 1);
    assert!(ledger.heads().is_empty(), "nothing was recorded, honestly");
}

#[tokio::test]
async fn a_cloud_with_no_ledger_ingests_anchors_like_any_other_event() {
    let cloud = Cloud::new(FakeStore::new());
    let outcome = cloud
        .ingest(&[anchor_event(1, 42, "ab")])
        .await
        .expect("ingest");
    assert_eq!(outcome.appended, 1);
}

#[tokio::test]
async fn a_run_of_shift_closes_walks_the_head_forward() {
    let ledger = Arc::new(FakeAnchors::default());
    let cloud = Cloud::new(FakeStore::new()).with_anchor_ledger(ledger.clone());

    cloud
        .ingest(&[
            anchor_event(1, 10, "aa"),
            anchor_event(2, 25, "bb"),
            anchor_event(3, 61, "cc"),
        ])
        .await
        .expect("ingest");

    assert_eq!(
        ledger.heads(),
        vec![
            (10, "aa".to_owned()),
            (25, "bb".to_owned()),
            (61, "cc".to_owned())
        ]
    );
    assert!(ledger.conflicts().is_empty());
}

// --- Recomputing the chain behind an anchor (ADR-0132) -------------------------------------------

/// A log the cloud holds, as the chain window reads it.
#[derive(Debug, Default)]
struct FakeWindow {
    events: Vec<EventEnvelope<RawPayload>>,
    /// When set, every read fails — the ingest must stand anyway.
    unreadable: bool,
}

impl ChainWindow for FakeWindow {
    fn events_in_window<'a>(
        &'a self,
        _tenant: TenantId,
        _store: StoreId,
        _day: &'a BusinessDate,
        _days_back: u8,
        limit: u32,
    ) -> pos_ports::dynamic::BoxFuture<'a, Result<Vec<EventEnvelope<RawPayload>>, AnchorError>>
    {
        if self.unreadable {
            return Box::pin(async { Err(AnchorError::new("the log is down")) });
        }
        let page: Vec<EventEnvelope<RawPayload>> =
            self.events.iter().take(limit as usize).cloned().collect();
        Box::pin(async move { Ok(page) })
    }
}

/// The hash of one record, the way the edge computes it.
fn record_hash(event: &EventEnvelope<RawPayload>, link: &ChainLink) -> ChainHash {
    event
        .chain_hash(link, |bytes| Sha256::digest(bytes).into())
        .expect("a fixture record hashes")
}

/// A correctly chained run of `count` records, exactly as an edge writes them.
///
/// Really hashed, not stamped with invented values: a fixture whose links do not hold would make
/// every "this log is sound" assertion vacuous.
fn honest_log(count: u32) -> Vec<EventEnvelope<RawPayload>> {
    let mut log = Vec::new();
    let mut link = ChainLink::first();
    for seed in 1..=count {
        let mut event = fixtures::activation(store_id(), seed);
        let hash = record_hash(&event, &link);
        event.chain = Some(link.clone());
        link = ChainLink::following(link.seq, hash);
        log.push(event);
    }
    log
}

/// The head of a chained run.
fn log_head(log: &[EventEnvelope<RawPayload>]) -> ChainHash {
    let last = log.last().expect("a non-empty log");
    record_hash(last, last.chain.as_ref().expect("a chained record"))
}

/// An anchor event sitting one position above `log`, reporting its head — what a shift close writes.
fn anchor_over(log: &[EventEnvelope<RawPayload>], seed: u32) -> EventEnvelope<RawPayload> {
    let head = log_head(log);
    let mut event = fixtures::envelope(
        store_id(),
        seed,
        &pos_proto::events::StoreChainAnchored {
            chain_seq: log.len() as u64,
            chain_head: head.clone(),
            unchained: 0,
        },
    );
    event.chain = Some(ChainLink::following(log.len() as u64, head));
    event
}

#[tokio::test]
async fn an_anchor_over_an_honest_log_is_accepted_and_the_chain_holds() {
    let log = honest_log(5);
    let ledger = Arc::new(FakeAnchors::default());
    let cloud = Cloud::new(FakeStore::new())
        .with_anchor_ledger(ledger.clone())
        .with_chain_window(Arc::new(FakeWindow {
            events: log.clone(),
            unreadable: false,
        }));

    cloud
        .ingest(&[anchor_over(&log, 100)])
        .await
        .expect("ingest");

    assert_eq!(
        ledger.heads(),
        vec![(5, log_head(&log).as_str().to_owned())],
        "the head is kept, and the records behind it reproduce it"
    );
    assert!(ledger.conflicts().is_empty());
}

#[tokio::test]
async fn an_anchor_claiming_a_chain_as_long_as_its_own_record_is_refused() {
    // Impossible: the anchor is written *into* the chain it describes, so it always sits above it.
    // Caught on one record, before anything is read.
    let ledger = Arc::new(FakeAnchors::default());
    let cloud = Cloud::new(FakeStore::new()).with_anchor_ledger(ledger.clone());

    let mut event = anchor_event(1, 42, "ab");
    event.chain = Some(ChainLink::following(41, ChainHash::from_hex("aa")));
    cloud.ingest(&[event]).await.expect("ingest");

    assert!(
        ledger.heads().is_empty(),
        "an impossible anchor never becomes a store's head"
    );
}

#[tokio::test]
async fn an_anchor_that_links_to_one_head_and_reports_another_is_refused_and_recorded() {
    let ledger = Arc::new(FakeAnchors::default());
    let cloud = Cloud::new(FakeStore::new()).with_anchor_ledger(ledger.clone());

    // Adjacent — seq 43 reporting length 42 — so `prev_hash` must be the head it reports.
    let mut event = anchor_event(1, 42, "ff");
    event.chain = Some(ChainLink::following(42, ChainHash::from_hex("aa")));
    cloud.ingest(&[event]).await.expect("ingest");

    assert!(ledger.heads().is_empty());
    let conflicts = ledger.conflicts();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].chain_seq, 42);
    assert_eq!(conflicts[0].held_head, ChainHash::from_hex("aa"));
    assert_eq!(conflicts[0].offered_head, ChainHash::from_hex("ff"));
}

#[tokio::test]
async fn a_busy_till_does_not_make_an_anchor_look_forged() {
    // The edge reads its head and *then* commits the anchor, so a sale landing in between leaves
    // the anchor several records above the length it names. This is a Friday night, not a fraud.
    let log = honest_log(8);
    let ledger = Arc::new(FakeAnchors::default());
    let cloud = Cloud::new(FakeStore::new())
        .with_anchor_ledger(ledger.clone())
        .with_chain_window(Arc::new(FakeWindow {
            events: log.clone(),
            unreadable: false,
        }));

    // Reports length 5, but sits at position 9 — four sales landed in between.
    let head_at_five = record_hash(&log[4], log[4].chain.as_ref().expect("chained"));
    let mut event = fixtures::envelope(
        store_id(),
        100,
        &pos_proto::events::StoreChainAnchored {
            chain_seq: 5,
            chain_head: head_at_five.clone(),
            unchained: 0,
        },
    );
    event.chain = Some(ChainLink::following(8, log_head(&log)));

    cloud.ingest(&[event]).await.expect("ingest");

    assert_eq!(
        ledger.heads(),
        vec![(5, head_at_five.as_str().to_owned())],
        "the anchor is accepted; nothing about it is a contradiction"
    );
    assert!(ledger.conflicts().is_empty());
}

#[tokio::test]
async fn a_window_the_cloud_cannot_read_never_stops_the_log() {
    let log = honest_log(4);
    let ledger = Arc::new(FakeAnchors::default());
    let cloud = Cloud::new(FakeStore::new())
        .with_anchor_ledger(ledger.clone())
        .with_chain_window(Arc::new(FakeWindow {
            events: Vec::new(),
            unreadable: true,
        }));

    let outcome = cloud
        .ingest(&[anchor_over(&log, 100)])
        .await
        .expect("ingest");

    assert_eq!(outcome.appended, 1);
    assert_eq!(
        ledger.heads().len(),
        1,
        "the head is still kept; only the recomputation was lost"
    );
}

#[tokio::test]
async fn a_cloud_with_no_window_still_keeps_and_refuses_heads() {
    // The two are composed apart on purpose: a deployment that cannot afford the window read still
    // refuses a forked head.
    let log = honest_log(3);
    let ledger = Arc::new(FakeAnchors::default());
    let cloud = Cloud::new(FakeStore::new()).with_anchor_ledger(ledger.clone());
    cloud
        .ingest(&[anchor_over(&log, 100)])
        .await
        .expect("ingest");
    assert_eq!(ledger.heads().len(), 1);
}

#[tokio::test]
async fn a_backfilled_anchor_does_not_re_walk_history_somebody_was_already_shown() {
    // A late anchor below the head describes records the anchor above it already covered.
    let log = honest_log(5);
    let ledger = Arc::new(FakeAnchors::holding(&[held_anchor(42, "ab")]));
    let cloud = Cloud::new(FakeStore::new())
        .with_anchor_ledger(ledger.clone())
        .with_chain_window(Arc::new(FakeWindow {
            events: log.clone(),
            unreadable: true,
        }));

    // The window is unreadable, so a walk would log an error; the point is that none is attempted.
    cloud
        .ingest(&[anchor_over(&log, 100)])
        .await
        .expect("ingest");

    assert_eq!(
        ledger.heads(),
        vec![
            (5, log_head(&log).as_str().to_owned()),
            (42, "ab".to_owned())
        ],
        "the backfill is filed and the head stays where it was"
    );
}

#[tokio::test]
async fn a_truncated_and_re_traded_log_is_caught_through_the_whole_path() {
    // **The case ADR-0131 could not close and ADR-0132 exists for**, and the one
    // `store-sqlite/tests/chain.rs::truncating_the_tail_is_not_caught_by_the_chain_alone` asserts a
    // bare chain misses.
    //
    // The store cut its log back and traded on, so the cloud now holds the original record at
    // position 4 and a different one claiming the same place. The anchor above it looks like honest
    // growth — nothing collides, its head is accepted — and the fork is found anyway, because the
    // events say what the anchors cannot.
    let log = honest_log(6);
    let mut with_a_second_history = log.clone();
    let mut impostor = fixtures::activation(store_id(), 900);
    impostor.chain = log[3].chain.clone();
    with_a_second_history.push(impostor);

    let ledger = Arc::new(FakeAnchors::default());
    let cloud = Cloud::new(FakeStore::new())
        .with_anchor_ledger(ledger.clone())
        .with_chain_window(Arc::new(FakeWindow {
            events: with_a_second_history,
            unreadable: false,
        }));

    cloud
        .ingest(&[anchor_over(&log, 100)])
        .await
        .expect("ingest");

    assert_eq!(
        ledger.heads(),
        vec![(6, log_head(&log).as_str().to_owned())],
        "the anchor itself is unremarkable — that is exactly why the anchors alone miss this"
    );
    let conflicts = ledger.conflicts();
    assert_eq!(
        conflicts.len(),
        1,
        "the contradiction is filed, not only logged: {conflicts:?}"
    );
    assert_eq!(conflicts[0].chain_seq, 4, "at the contested position");
    assert_ne!(
        conflicts[0].held_head, conflicts[0].offered_head,
        "two records at one position hash to two different answers, and the pair is the finding"
    );
    assert_eq!(
        conflicts[0].offered_event_id,
        fixtures::event_id(900),
        "the finding names the record that claimed a place already taken"
    );
}

#[tokio::test]
async fn an_edited_record_is_caught_through_the_whole_path() {
    // The cloud re-derives the chain from its own copies rather than trusting that the store did.
    let mut log = honest_log(5);
    let head = log_head(&log);
    let anchor = anchor_over(&log, 100);
    // Somebody edited record 3 after the fact. Record 4 still links to what 3 used to be.
    log[2].data = RawPayload::encode(&pos_proto::events::DeviceActivationCompleted {
        activated_device_id: DeviceId::new(Ulid::from_u128(0xDEAD)),
    })
    .expect("a fixture payload serialises");

    let ledger = Arc::new(FakeAnchors::default());
    let cloud = Cloud::new(FakeStore::new())
        .with_anchor_ledger(ledger.clone())
        .with_chain_window(Arc::new(FakeWindow {
            events: log,
            unreadable: false,
        }));

    cloud.ingest(&[anchor]).await.expect("ingest");

    assert_eq!(ledger.heads(), vec![(5, head.as_str().to_owned())]);
    let conflicts = ledger.conflicts();
    assert!(
        conflicts.iter().any(|conflict| conflict.chain_seq == 4),
        "record 4 links to a record 3 that no longer exists: {conflicts:?}"
    );
}

#[tokio::test]
async fn a_gap_in_the_window_is_never_filed_as_a_contradiction() {
    // A broker gap. ADR-0040 exists to fill exactly this, and filing it here would put an
    // accusation in the record because the recovery mechanism was doing its job.
    let log = honest_log(6);
    let mut with_a_gap = log.clone();
    with_a_gap.remove(2);

    let ledger = Arc::new(FakeAnchors::default());
    let cloud = Cloud::new(FakeStore::new())
        .with_anchor_ledger(ledger.clone())
        .with_chain_window(Arc::new(FakeWindow {
            events: with_a_gap,
            unreadable: false,
        }));

    cloud
        .ingest(&[anchor_over(&log, 100)])
        .await
        .expect("ingest");

    assert_eq!(ledger.heads().len(), 1, "the head is still kept");
    assert!(
        ledger.conflicts().is_empty(),
        "a gap is a prompt to reconcile, never a contradiction to record"
    );
}

#[tokio::test]
async fn a_fork_raises_one_critical_alert_and_a_gap_raises_none() {
    // The alert reads what ingest filed (ADR-0073 amendment 2026-10-31), so what ADR-0132 decision 5
    // treats as an accusation is exactly what can raise it: a fork does, a broker gap does not.
    async fn chain_alerts_after(
        window: Vec<EventEnvelope<RawPayload>>,
        anchor_log: &[EventEnvelope<RawPayload>],
    ) -> Vec<pos_cloud::alerts::FiringAlert> {
        let ledger = Arc::new(FakeAnchors::default());
        let cloud = Cloud::new(FakeStore::new())
            .with_anchor_ledger(ledger.clone())
            .with_chain_window(Arc::new(FakeWindow {
                events: window,
                unreadable: false,
            }));
        cloud
            .ingest(&[anchor_over(anchor_log, 100)])
            .await
            .expect("ingest");
        pos_cloud::alerts::chain::chain_alerts(
            Some(ledger.as_ref()),
            &crate::alerts::FakeAlerts::default(),
            &[tenant()],
        )
        .await
        .expect("the alert store answers")
    }

    let log = honest_log(6);
    let mut with_a_gap = log.clone();
    with_a_gap.remove(2);
    assert!(
        chain_alerts_after(with_a_gap, &log).await.is_empty(),
        "a gap is a prompt to reconcile, never an alert"
    );

    let mut forked = log.clone();
    let mut impostor = fixtures::activation(store_id(), 900);
    impostor.chain = log[3].chain.clone();
    forked.push(impostor);
    let firing = chain_alerts_after(forked, &log).await;
    assert_eq!(firing.len(), 1, "one alert for the store: {firing:?}");
    assert_eq!(firing[0].kind, pos_cloud::alerts::AlertKind::ChainBroken);
    assert_eq!(
        firing[0].severity,
        pos_cloud::alerts::AlertSeverity::Critical
    );
    assert_eq!(firing[0].dedup_key, store_id().to_string());
    assert_eq!(firing[0].detail["from_seq"], 4, "at the contested position");
}

/// The cloud's own copy of a store's log, as the chain window reads it in production: what ingest
/// actually stored, owner stamp and all, rather than a list the test wrote.
///
/// One adjustment, and it is the fake's rather than the cloud's: `FakeStore` chains whatever it is
/// given, as a store's own database does, so it overwrites the box's links with its own. The
/// cloud's PostgreSQL log keeps each envelope verbatim, links included, so the window puts back the
/// links the box sent — and keeps everything ingest changed.
#[derive(Debug)]
struct HeldWindow {
    store: FakeStore,
    sent: Vec<EventEnvelope<RawPayload>>,
}

impl ChainWindow for HeldWindow {
    fn events_in_window<'a>(
        &'a self,
        _tenant: TenantId,
        store: StoreId,
        _day: &'a BusinessDate,
        _days_back: u8,
        limit: u32,
    ) -> pos_ports::dynamic::BoxFuture<'a, Result<Vec<EventEnvelope<RawPayload>>, AnchorError>>
    {
        Box::pin(async move {
            let page =
                core::num::NonZeroU32::new(limit.max(1)).unwrap_or(core::num::NonZeroU32::MIN);
            let held = pos_ports::EventStore::read(
                &self.store,
                &pos_ports::event_store::EventQuery::first(store, page),
            )
            .await
            .map_err(|error| AnchorError::new(error.to_string()))?;
            Ok(held
                .into_iter()
                .map(|mut event| {
                    event.chain = self
                        .sent
                        .iter()
                        .find(|sent| sent.event_id == event.event_id)
                        .and_then(|sent| sent.chain.clone());
                    event
                })
                .collect())
        })
    }
}

/// A box publishes its chained log and then a shift-close anchor over HTTPS, the cloud stamps the
/// registry's tenant and brand on every record (ADR-0101), and the chain still audits clean (pilot
/// run 2, N4: every link from seq 2 read as broken, 61 findings for an honest store).
#[tokio::test]
async fn a_chain_published_over_https_and_stamped_with_its_owner_audits_clean() {
    // As a store writes it: the nil tenant and brand, because it does not know who owns it.
    let mut log = Vec::new();
    let mut link = ChainLink::first();
    for seed in 1..=5 {
        let mut event = fixtures::activation(store_id(), seed);
        event.tenant_id = TenantId::new(Ulid::NIL);
        event.brand_id = pos_proto::ids::BrandId::new(Ulid::NIL);
        let hash = record_hash(&event, &link);
        event.chain = Some(link.clone());
        link = ChainLink::following(link.seq, hash);
        log.push(event);
    }
    let mut anchor = anchor_over(&log, 100);
    anchor.tenant_id = TenantId::new(Ulid::NIL);
    anchor.brand_id = pos_proto::ids::BrandId::new(Ulid::NIL);

    let store = FakeStore::new();
    let ledger = Arc::new(FakeAnchors::default());
    let cloud = Cloud::with_store_owners(
        store.clone(),
        Arc::new(crate::FakeOwners {
            store: store_id(),
            owner: pos_cloud::StoreOwner {
                tenant_id: tenant(),
                brand_id: pos_proto::ids::BrandId::new(Ulid::from_u128(0x000B_4A4D)),
            },
        }),
    )
    .with_anchor_ledger(ledger.clone())
    .with_chain_window(Arc::new(HeldWindow {
        store: store.clone(),
        sent: log.iter().cloned().chain([anchor.clone()]).collect(),
    }));
    let keys = FakeKeys::default();
    let router = http::router(app_all(
        cloud,
        FakeRollups::default(),
        keys.clone(),
        provisioned_admin(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    ));
    let token = crate::issue_device_credential(&keys, tenant(), store_id());
    let uri = format!("/sync/stores/{}/events", store_id().as_ulid());
    for batch in [log.clone(), vec![anchor]] {
        let published = router
            .clone()
            .oneshot(crate::post_json_bearer(
                &uri,
                &serde_json::json!({ "events": batch }),
                &token,
            ))
            .await
            .expect("route the publish");
        assert_eq!(published.status(), StatusCode::OK);
    }

    let held = read_back(&store).await;
    assert!(
        held.iter().all(|event| event.tenant_id == tenant()),
        "the cloud filed every record under the registry's tenant"
    );
    assert_eq!(
        ledger.heads(),
        vec![(5, log_head(&log).as_str().to_owned())],
        "the head the box reported is held"
    );
    assert!(
        ledger.conflicts().is_empty(),
        "an honest chain is not filed as tampering: {:?}",
        ledger.conflicts()
    );
}

// --- The chain-finding read (`GET /admin/chain-findings`) ----------------------------------------

/// The console read over an anchor ledger, with the session guard in front of it.
fn chain_app(admin: FakeAdmin, anchors: Arc<FakeAnchors>) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::chain_router(anchors, admin, clock()))
}

#[tokio::test]
async fn the_chain_findings_read_lists_what_the_cloud_refused() {
    // The whole point of the mechanism is being noticed. Until this route existed a finding reached
    // a human only as a line in the server's log and a row nobody could read.
    let anchors = Arc::new(FakeAnchors::holding(&[held_anchor(42, "ab")]));
    let cloud = Cloud::new(FakeStore::new()).with_anchor_ledger(anchors.clone());
    // A store offers a second, different head at a length the cloud already holds.
    cloud
        .ingest(&[anchor_event(1, 42, "ff")])
        .await
        .expect("ingest");

    let router = chain_app(provisioned_admin(), anchors);
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();

    let listed = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/admin/chain-findings?tenant_id={tenant_ulid}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .expect("build the request"),
        )
        .await
        .expect("route the findings read");
    assert_eq!(listed.status(), StatusCode::OK);

    let findings = json_body(listed).await;
    let findings = findings.as_array().expect("a findings array");
    assert_eq!(
        findings.len(),
        1,
        "the refusal the ingest recorded is listed"
    );
    assert_eq!(findings[0]["chain_seq"], 42);
    assert_eq!(
        findings[0]["held_head"], "ab",
        "what the cloud holds, and what it was offered against it — the pair is the finding"
    );
    assert_eq!(findings[0]["offered_head"], "ff");
    assert!(
        findings[0]["offered_event_id"].is_string(),
        "the finding traces back to a record"
    );
}

#[tokio::test]
async fn a_clean_fleet_reads_as_an_empty_list_rather_than_an_error() {
    // The expected state. A screen that erred when nothing was wrong would teach an operator to
    // ignore it.
    let router = chain_app(provisioned_admin(), Arc::new(FakeAnchors::default()));
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();

    let listed = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/admin/chain-findings?tenant_id={tenant_ulid}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .expect("build the request"),
        )
        .await
        .expect("route the findings read");
    assert_eq!(listed.status(), StatusCode::OK);
    assert!(
        json_body(listed)
            .await
            .as_array()
            .expect("an array")
            .is_empty()
    );
}

#[tokio::test]
async fn the_chain_findings_read_needs_a_session() {
    let router = chain_app(provisioned_admin(), Arc::new(FakeAnchors::default()));
    let tenant_ulid = tenant().as_ulid().to_string();
    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/admin/chain-findings?tenant_id={tenant_ulid}"))
                .body(Body::empty())
                .expect("build the request"),
        )
        .await
        .expect("route the findings read");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a store's chain findings are its tenant's data, not a public read"
    );
}

#[tokio::test]
async fn the_chain_findings_read_refuses_a_tenant_that_is_not_a_ulid() {
    let router = chain_app(provisioned_admin(), Arc::new(FakeAnchors::default()));
    let cookie = admin_cookie(&router).await;
    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/chain-findings?tenant_id=not-a-ulid")
                .header("cookie", &cookie)
                .body(Body::empty())
                .expect("build the request"),
        )
        .await
        .expect("route the findings read");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
