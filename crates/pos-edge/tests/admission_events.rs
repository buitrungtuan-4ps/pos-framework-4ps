// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud learns what a store admitted
//! ([ADR-0118](../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md) §4).
//!
//! Until this, a tablet paired during a WAN outage was invisible to the cloud **forever**: the
//! heartbeat carries liveness and outbox depth, the `/sync` report carries the installed version,
//! and the catalogue's only device event was activation. So the fleet console's picture of a store's
//! tills was zero rows, a lost tablet meant sending somebody to the store, and a remote revocation
//! had nothing to name.
//!
//! These tests are about the **shape** of what now reaches the cloud, because that shape is a
//! compliance decision as much as a design one. Two things are asserted on every admission: that the
//! event is on the store's durable log, and that its payload names devices and **no employee**. The
//! second is the one that would be quietly lost in a refactor, and the one ADR-0118's *Compliance
//! posture* section exists for.

use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

use pos_edge::admission_events::AdmissionEvents;
use pos_edge::pairing::{Minter, Pairing};
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::device_registry::DeviceRegistry;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::ClockSource;
use pos_proto::ids::{DeviceId, EmployeeId, StoreId};
use pos_proto::ulid::Ulid;

/// The store every fixture below trades as.
fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(1))
}

/// A composed edge on a fake store — the same object `serve` hands to `Pairing` in production.
fn edge() -> Arc<Edge<FakeStore>> {
    Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(store_id()),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed"),
    )
}

/// Every event on the store's log, as `(token, payload)` — what the outbox drain will carry to the
/// cloud, read back through the port rather than through the fan-out, because the durability claim
/// is the whole point of using the outbox.
async fn logged(edge: &Edge<FakeStore>) -> Vec<(String, serde_json::Value)> {
    let query = EventQuery::first(store_id(), NonZeroU32::new(64).expect("non-zero"));
    edge.store()
        .read(&query)
        .await
        .expect("read the log")
        .into_iter()
        .map(|envelope| {
            let payload =
                serde_json::from_str(envelope.data.as_json()).unwrap_or(serde_json::Value::Null);
            (envelope.event_type.as_str().to_owned(), payload)
        })
        .collect()
}

/// The one event of `token`, or a panic naming what was on the log instead.
fn only(events: &[(String, serde_json::Value)], token: &str) -> serde_json::Value {
    let matching: Vec<&serde_json::Value> = events
        .iter()
        .filter(|(name, _)| name == token)
        .map(|(_, payload)| payload)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "expected exactly one {token}; the log holds {:?}",
        events.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
    matching.first().copied().cloned().unwrap_or_default()
}

/// A manager's mint, then a redemption, names both devices and no person.
#[test]
fn an_admission_names_the_admitting_till_and_no_employee() {
    run_ready(async {
        let edge = edge();
        let pairing = Pairing::new().with_admission_events(Some(
            Arc::<Edge<FakeStore>>::clone(&edge) as Arc<dyn AdmissionEvents>,
        ));
        let now = pos_edge::SystemClock.now();

        let till = DeviceId::new(Ulid::from_u128(77));
        let manager = EmployeeId::new(Ulid::from_u128(9));
        let (code, _) = pairing
            .mint(
                now,
                Minter::Manager {
                    device_id: till,
                    employee_id: manager,
                },
            )
            .expect("mint");
        let redeemed = pairing.redeem(&code, now).await.expect("redeem");
        let admitted = pairing
            .device_for(&redeemed.token().expect("a fresh code pairs a device"))
            .expect("the token resolves to the admitted device");

        let events = logged(&edge).await;
        let payload = only(&events, "device.admission.granted");
        assert_eq!(
            payload.get("admitted_device_id").and_then(|v| v.as_str()),
            Some(admitted.to_string().as_str()),
            "the event names the device that was admitted: {payload}"
        );
        assert_eq!(
            payload
                .get("admitted_by_device_id")
                .and_then(|v| v.as_str()),
            Some(till.to_string().as_str()),
            "and the paired till whose manager minted the code: {payload}"
        );
        // The load-bearing assertion. An employee identity here would be a durable, central,
        // cross-border, attributable record of managerial activity, needing a lawful basis, a
        // retention period, a DPIA and a transfer basis before it could be written at all — and the
        // purpose this event serves is served in full without it (ADR-0118 §5).
        let serialised = payload.to_string();
        assert!(
            !serialised.contains(&manager.to_string()),
            "the minting employee must not reach the cloud-bound event: {serialised}"
        );
        assert_eq!(
            payload.as_object().map(serde_json::Map::len),
            Some(2),
            "two fields, both device ids: {payload}"
        );
    });
}

/// The employee *is* recorded — on the store's own disk, which is the other half of the split.
#[test]
fn the_admitting_employee_is_recorded_locally_and_only_locally() {
    run_ready(async {
        let store = FakeStore::default();
        let edge = Arc::new(
            Edge::new(
                store.clone(),
                StoreIdentity::for_store(store_id()),
                EdgeSession::bootstrap(),
                Arc::new(InMemoryReceipts::new()),
            )
            .expect("seed"),
        );
        let registry: Arc<dyn pos_edge::durable_auth::DurableAuth> = Arc::new(store.clone());
        let pairing =
            Pairing::durable(registry)
                .with_admission_events(Some(
                    Arc::<Edge<FakeStore>>::clone(&edge) as Arc<dyn AdmissionEvents>
                ));
        let now = pos_edge::SystemClock.now();

        let manager = EmployeeId::new(Ulid::from_u128(9));
        let (code, _) = pairing
            .mint(
                now,
                Minter::Manager {
                    device_id: DeviceId::new(Ulid::from_u128(77)),
                    employee_id: manager,
                },
            )
            .expect("mint");
        pairing.redeem(&code, now).await.expect("redeem");

        let admitted = store.paired_devices().await.expect("read the registry");
        assert_eq!(admitted.len(), 1);
        assert_eq!(
            admitted.first().and_then(|device| device.admitted_by),
            Some(manager),
            "the store's own row answers who let this tablet in"
        );
    });
}

/// The code a boot announces is authorised by nobody, and the event says so rather than inventing a
/// device.
#[test]
fn the_boot_code_admits_with_no_authorising_device() {
    run_ready(async {
        let edge = edge();
        let pairing = Pairing::new().with_admission_events(Some(
            Arc::<Edge<FakeStore>>::clone(&edge) as Arc<dyn AdmissionEvents>,
        ));
        let now = pos_edge::SystemClock.now();

        let (code, _) = pairing.mint(now, Minter::Boot).expect("mint");
        pairing.redeem(&code, now).await.expect("redeem");

        let payload = only(&logged(&edge).await, "device.admission.granted");
        assert!(
            payload
                .get("admitted_by_device_id")
                .is_some_and(serde_json::Value::is_null),
            "the first device on a virgin box was admitted by nothing, because nothing could: \
             {payload}"
        );
    });
}

/// Retiring the fleet emits one event per device, not one event meaning *all of them*.
///
/// The alternative — an absent `revoked_device_id` standing for the whole store — would give the
/// cloud's reader two shapes to handle and one of them would be a `WHERE store_id =` sweep. One
/// event per device keeps the reader to a single arm, and the count is bounded by what a store
/// actually paired.
#[test]
fn the_break_glass_emits_one_revocation_per_device() {
    run_ready(async {
        let edge = edge();
        let pairing = Pairing::new().with_admission_events(Some(
            Arc::<Edge<FakeStore>>::clone(&edge) as Arc<dyn AdmissionEvents>,
        ));
        let now = pos_edge::SystemClock.now();

        for _ in 0..3 {
            let (code, _) = pairing.mint(now, Minter::Boot).expect("mint");
            pairing.redeem(&code, now).await.expect("redeem");
        }
        let paired: Vec<DeviceId> = pairing
            .paired_devices()
            .into_iter()
            .map(|(device_id, _)| device_id)
            .collect();
        assert_eq!(paired.len(), 3, "three devices to retire");

        pairing
            .revoke_all()
            .await
            .expect("no registry, so no failure");

        let events = logged(&edge).await;
        let mut revoked: Vec<String> = events
            .iter()
            .filter(|(token, _)| token == "device.admission.revoked")
            .filter_map(|(_, payload)| {
                payload
                    .get("revoked_device_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
            })
            .collect();
        revoked.sort();
        let mut expected: Vec<String> = paired.iter().map(DeviceId::to_string).collect();
        expected.sort();
        assert_eq!(revoked, expected, "every retired device is named, once");
    });
}

/// Retiring one device names that device and leaves the others alone.
#[test]
fn revoking_one_device_names_it() {
    run_ready(async {
        let edge = edge();
        let pairing = Pairing::new().with_admission_events(Some(
            Arc::<Edge<FakeStore>>::clone(&edge) as Arc<dyn AdmissionEvents>,
        ));
        let now = pos_edge::SystemClock.now();

        let (code, _) = pairing.mint(now, Minter::Boot).expect("mint");
        let token = pairing
            .redeem(&code, now)
            .await
            .expect("redeem")
            .token()
            .expect("a fresh code pairs a device");
        let device = pairing.device_for(&token).expect("resolves");

        pairing
            .revoke(device)
            .await
            .expect("no registry, so no failure");

        let payload = only(&logged(&edge).await, "device.admission.revoked");
        assert_eq!(
            payload.get("revoked_device_id").and_then(|v| v.as_str()),
            Some(device.to_string().as_str()),
            "the event names the device whose token stops working: {payload}"
        );
        assert_eq!(
            payload.as_object().map(serde_json::Map::len),
            Some(1),
            "one field, a device id: {payload}"
        );
    });
}

/// A reporter that always fails must not cost the store a pairing.
///
/// This is the failure mode worth pinning: refusing an admission because an observability event
/// could not be written would hand an operator a broken pairing screen in the middle of service, to
/// protect a console column. The event is best-effort and says so in three places; this proves it.
#[test]
fn a_failing_reporter_does_not_refuse_the_pairing_or_the_revocation() {
    /// Counts what it was asked to report, and refuses everything.
    #[derive(Default)]
    struct AlwaysFails {
        calls: Mutex<usize>,
    }

    impl AdmissionEvents for AlwaysFails {
        fn device_admitted(
            &self,
            _admitted: DeviceId,
            _admitted_by: Option<DeviceId>,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), pos_edge::app::AppError>> + Send + '_>>
        {
            Box::pin(async {
                *self.calls.lock().expect("not poisoned") += 1;
                Err(pos_edge::app::AppError::Clock)
            })
        }

        fn device_revoked(
            &self,
            _revoked: DeviceId,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), pos_edge::app::AppError>> + Send + '_>>
        {
            Box::pin(async {
                *self.calls.lock().expect("not poisoned") += 1;
                Err(pos_edge::app::AppError::Clock)
            })
        }
    }

    run_ready(async {
        let reporter = Arc::new(AlwaysFails::default());
        let pairing = Pairing::new().with_admission_events(Some(
            Arc::<AlwaysFails>::clone(&reporter) as Arc<dyn AdmissionEvents>,
        ));
        let now = pos_edge::SystemClock.now();

        let (code, _) = pairing.mint(now, Minter::Boot).expect("mint");
        let token = pairing
            .redeem(&code, now)
            .await
            .expect("a reporter that refuses must not refuse the pairing")
            .token()
            .expect("the device still gets its token");
        let device = pairing.device_for(&token).expect("and it still resolves");
        pairing
            .revoke(device)
            .await
            .expect("nor may it undo a revocation");

        assert!(
            pairing.device_for(&token).is_none(),
            "the revocation stands even though the cloud was never told"
        );
        assert_eq!(
            *reporter.calls.lock().expect("not poisoned"),
            2,
            "both the admission and the revocation were offered to the reporter"
        );
    });
}
