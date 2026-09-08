// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The console retires a till on a store nobody can reach
//! ([ADR-0118](../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md) §6).
//!
//! `device_revocations`'s own unit tests cover the decision in isolation. These cover the three
//! claims that only hold once the decision is wired to a real `Pairing` over a real registry, and
//! each of them is a claim a refactor could quietly break:
//!
//! 1. **The token actually stops working.** Writing the registry alone would delete the row and
//!    leave `Pairing`'s in-memory digest map — which the request gate answers from — untouched, so
//!    the stolen tablet would keep trading until the next restart.
//! 2. **It happens exactly once.** The node stays in the document forever, so without the durable
//!    applied record the box would re-revoke and re-report every poll, for ever.
//! 3. **A refusal changes nothing at all.** Not "most of it" — the blast-radius caps are only worth
//!    having if the till they refuse to retire is still trading afterwards.

use std::num::NonZeroU32;
use std::sync::Arc;

use pos_edge::device_revocations::DeviceRevocations;
use pos_edge::durable_auth::{DurableAuth, EdgeRegistry};
use pos_edge::pairing::{Minter, Pairing};
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::device_registry::DeviceRegistry;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::ClockSource;
use pos_proto::ids::{DeviceId, StoreId};
use pos_proto::ulid::Ulid;

/// The store every fixture below trades as.
fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(1))
}

/// A composed edge, the pairing state over its registry, and the deny-list carrier over both —
/// exactly the three objects `compose` builds, in the same order and over the same seam.
fn fixture() -> (Arc<Edge<FakeStore>>, Arc<Pairing>, Arc<DeviceRevocations>) {
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(store_id()),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed"),
    );
    let registry: Arc<dyn DurableAuth> = Arc::new(EdgeRegistry(Arc::clone(&edge)));
    let pairing = Arc::new(
        Pairing::durable(Arc::clone(&registry))
            .with_admission_events(Some(Arc::<Edge<FakeStore>>::clone(&edge))),
    );
    let revocations = Arc::new(DeviceRevocations::new(
        Arc::clone(&pairing),
        Arc::clone(&registry),
    ));
    (edge, pairing, revocations)
}

/// Pairs one device and hands back its id and its token digest resolver input.
async fn pair(pairing: &Pairing) -> (DeviceId, pos_ports::device_registry::TokenDigest) {
    let now = pos_edge::SystemClock.now();
    let (code, _) = pairing.mint(now, Minter::Boot).expect("mint");
    let redeemed = pairing.redeem(&code, now).await.expect("redeem");
    let token = redeemed.token().expect("a fresh code pairs a device");
    let device_id = pairing
        .device_for(&token)
        .expect("the token resolves to the admitted device");
    (device_id, token.digest())
}

/// A document publishing a deny-list of the given ids.
fn document(ids: &[DeviceId]) -> serde_json::Value {
    let listed = ids.iter().map(ToString::to_string).collect::<Vec<_>>();
    serde_json::json!({ "revoked_devices": { "device_ids": listed } })
}

/// How many revocation events the store's log holds.
async fn revocation_events(edge: &Edge<FakeStore>) -> usize {
    let query = EventQuery::first(store_id(), NonZeroU32::new(64).expect("non-zero"));
    edge.store()
        .read(&query)
        .await
        .expect("read the log")
        .into_iter()
        .filter(|envelope| envelope.event_type.as_str() == "device.admission.revoked")
        .count()
}

#[test]
fn a_published_deny_list_stops_the_token_resolving_and_tells_the_cloud_once() {
    run_ready(async {
        let (edge, pairing, revocations) = fixture();
        let (lost, lost_digest) = pair(&pairing).await;
        // A second till, so retiring the first does not leave the store with nothing admitted.
        let (kept, kept_digest) = pair(&pairing).await;

        let retired = revocations
            .apply(&document(&[lost]))
            .await
            .expect("the applied record reads");
        assert_eq!(retired, 1);

        // The in-memory map, not the table: this is what the request gate answers from, and the
        // whole reason the carrier holds `Pairing` rather than the registry seam.
        assert!(
            pairing.paired_devices().iter().all(|(id, _)| *id != lost),
            "the retired till is gone from the live pairing state"
        );
        assert!(
            pairing.paired_devices().iter().any(|(id, _)| *id == kept),
            "and the other till is untouched"
        );
        assert_eq!(
            DeviceRegistry::device_for_token(edge.store(), lost_digest)
                .await
                .expect("read the registry"),
            None,
            "the durable row is gone too, so a restart cannot bring it back"
        );
        assert_eq!(
            DeviceRegistry::device_for_token(edge.store(), kept_digest)
                .await
                .expect("read the registry"),
            Some(kept)
        );
        assert_eq!(
            revocation_events(&edge).await,
            1,
            "the cloud is told exactly once"
        );

        // The node stays in the document for ever. Re-applying it must do nothing at all — not a
        // write, not an event. Without the durable applied record this would fire every thirty
        // seconds until somebody noticed the outbox.
        let again = revocations
            .apply(&document(&[lost]))
            .await
            .expect("the applied record reads");
        assert_eq!(again, 0);
        assert_eq!(
            revocation_events(&edge).await,
            1,
            "and never again, however many times the same document arrives"
        );
    });
}

#[test]
fn a_document_that_would_close_the_whole_floor_is_refused_whole() {
    run_ready(async {
        let (edge, pairing, revocations) = fixture();
        let (first, first_digest) = pair(&pairing).await;
        let (second, _) = pair(&pairing).await;

        let retired = revocations
            .apply(&document(&[first, second]))
            .await
            .expect("the applied record reads");
        assert_eq!(retired, 0, "neither till is retired");
        assert_eq!(
            pairing.paired_devices().len(),
            2,
            "a refusal is whole: not one of the two, and not the first of the two"
        );
        assert_eq!(
            DeviceRegistry::device_for_token(edge.store(), first_digest)
                .await
                .expect("read the registry"),
            Some(first),
            "the till the list named first is still trading"
        );
        assert_eq!(revocation_events(&edge).await, 0);
        assert_eq!(
            DeviceRegistry::revocations_applied(edge.store())
                .await
                .expect("read the applied record"),
            Vec::new(),
            "and nothing is recorded, so a corrected document can still be applied"
        );
    });
}

#[test]
fn retiring_a_store_s_only_till_is_refused_whole() {
    run_ready(async {
        let (edge, pairing, revocations) = fixture();
        let (only, only_digest) = pair(&pairing).await;

        let retired = revocations
            .apply(&document(&[only]))
            .await
            .expect("the applied record reads");
        assert_eq!(retired, 0);
        assert_eq!(
            DeviceRegistry::device_for_token(edge.store(), only_digest)
                .await
                .expect("read the registry"),
            Some(only),
            "a store left with nothing admitted could not sell, and could not pair a replacement \
             without somebody physically at the box"
        );
        assert_eq!(revocation_events(&edge).await, 0);
    });
}

#[test]
fn an_id_this_store_never_had_is_recorded_without_an_event() {
    run_ready(async {
        let (edge, pairing, revocations) = fixture();
        let (_kept, _) = pair(&pairing).await;
        let elsewhere = DeviceId::new(Ulid::from_u128(999));

        let retired = revocations
            .apply(&document(&[elsewhere]))
            .await
            .expect("the applied record reads");
        assert_eq!(retired, 0, "there was nothing here to retire");
        assert_eq!(
            revocation_events(&edge).await,
            0,
            "and no second event for an admission the cloud already knows ended"
        );
        assert_eq!(
            DeviceRegistry::revocations_applied(edge.store())
                .await
                .expect("read the applied record"),
            vec![elsewhere],
            "but it is recorded, so the store stops re-deciding it every poll"
        );
    });
}

#[test]
fn a_document_that_drops_the_entry_does_not_hand_the_tablet_back() {
    run_ready(async {
        let (edge, pairing, revocations) = fixture();
        let (lost, lost_digest) = pair(&pairing).await;
        let (_kept, _) = pair(&pairing).await;
        revocations
            .apply(&document(&[lost]))
            .await
            .expect("the applied record reads");

        // A config rollback restores an older effective document — the hazard §6 is built against.
        // Nothing in the shorter document can undo a deleted pairing row.
        revocations
            .apply(&document(&[]))
            .await
            .expect("the applied record reads");
        assert_eq!(
            DeviceRegistry::device_for_token(edge.store(), lost_digest)
                .await
                .expect("read the registry"),
            None,
            "a rollback un-says the instruction, and the instruction was already carried out"
        );
        assert!(pairing.paired_devices().iter().all(|(id, _)| *id != lost));
    });
}

#[test]
fn a_malformed_node_retires_nothing() {
    run_ready(async {
        let (edge, pairing, revocations) = fixture();
        let (only, only_digest) = pair(&pairing).await;
        let (_second, _) = pair(&pairing).await;

        let retired = revocations
            .apply(&serde_json::json!({
                "revoked_devices": { "device_ids": [only.to_string(), "not-a-ulid"] }
            }))
            .await
            .expect("the applied record reads");
        assert_eq!(
            retired, 0,
            "half a deny-list is a security control with a hole in it, so the node is refused whole"
        );
        assert_eq!(
            DeviceRegistry::device_for_token(edge.store(), only_digest)
                .await
                .expect("read the registry"),
            Some(only)
        );
        assert_eq!(revocation_events(&edge).await, 0);
    });
}
