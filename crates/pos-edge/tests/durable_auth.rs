// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A restart no longer unpairs the store, and no longer signs everyone out
//! ([ADR-0091](../../../docs/adr/0091-durable-edge-auth-state.md), roadmap v3 slice **S0d**).
//!
//! These are the tests that would have caught the original defect, so they are written the way it
//! actually failed: pair a device, **throw the whole in-memory state away**, build fresh state over
//! the same registry, and check that the token the device is still holding works. Before S0d that
//! second step erased everything, and the only symptom was an operator walking to the console with
//! a queue building behind them.
//!
//! `FakeStore` is the registry, which is exactly what makes the round trip meaningful: `reopen()`
//! shares the same rows, so "restart" here means what it means in the field — the process state is
//! gone, the store's database is not.

#![allow(
    clippy::expect_used,
    reason = "test scaffolding: a failed setup is an unrecoverable test fault"
)]

use std::sync::Arc;

use pos_edge::durable_auth::DurableAuth;
use pos_edge::pairing::{DeviceToken, Pairing};
use pos_edge::{Sessions, SystemClock};
use pos_fakes::FakeStore;
use pos_proto::ClockSource;
use pos_proto::ids::EmployeeId;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;

/// A registry backed by a fake store — the same handle both "processes" share, like the file on a
/// real box.
fn registry(store: &FakeStore) -> Arc<dyn DurableAuth> {
    Arc::new(store.clone())
}

fn at(ms: i64) -> Timestamp {
    Timestamp::from_milliseconds_since_epoch(ms).expect("a valid instant")
}

/// Pairs a device against `pairing`, returning the token the device would hold.
async fn pair(pairing: &Pairing, now: Timestamp) -> DeviceToken {
    let code = pairing.mint(now).expect("mint a pairing code");
    pairing
        .redeem(&code, now)
        .await
        .expect("redeem succeeds")
        .token()
        .expect("a live code yields a token")
}

#[tokio::test]
async fn a_paired_device_survives_a_restart() {
    let store = FakeStore::new();
    let now = SystemClock.now();

    // First boot: a device pairs.
    let token = {
        let pairing = Pairing::durable(registry(&store));
        let token = pair(&pairing, now).await;
        assert!(pairing.device_for(&token).is_some());
        token
    };

    // The process restarts: brand-new state, same store.
    let after = Pairing::durable(registry(&store));
    assert!(
        after.device_for(&token).is_none(),
        "before loading, fresh state knows nothing — which is what a restart used to leave"
    );
    let restored = after.load().await.expect("the registry reads");
    assert_eq!(restored, 1, "one device restored");
    assert!(
        after.device_for(&token).is_some(),
        "the token the device is still holding works after the restart"
    );
}

#[tokio::test]
async fn an_in_memory_pairing_still_loses_everything() {
    // The pre-S0d behaviour, kept as a test so the difference is visible rather than asserted in
    // prose — and so a fork that composes no registry knows what it is choosing.
    let now = SystemClock.now();
    let first = Pairing::new();
    let token = pair(&first, now).await;
    assert!(first.device_for(&token).is_some());
    assert!(!first.is_durable());

    let after = Pairing::new();
    assert_eq!(after.load().await.expect("no registry is not an error"), 0);
    assert!(
        after.device_for(&token).is_none(),
        "with no registry a restart unpairs every device, exactly as before S0d"
    );
}

#[tokio::test]
async fn a_revoked_device_stays_revoked_across_a_restart() {
    // The other half of making revocation explicit: it has to outlive the process, or "revoked"
    // would mean "until the next reboot".
    let store = FakeStore::new();
    let now = SystemClock.now();

    let pairing = Pairing::durable(registry(&store));
    let token = pair(&pairing, now).await;
    let device = pairing.device_for(&token).expect("resolves");
    pairing.revoke(device).await.expect("revoke writes through");

    let after = Pairing::durable(registry(&store));
    after.load().await.expect("the registry reads");
    assert!(
        after.device_for(&token).is_none(),
        "a revoked device must not come back at the next restart"
    );
}

#[tokio::test]
async fn revoke_all_is_the_break_glass_and_it_is_durable() {
    let store = FakeStore::new();
    let now = SystemClock.now();

    let pairing = Pairing::durable(registry(&store));
    let one = pair(&pairing, now).await;
    let two = pair(&pairing, now).await;
    assert_eq!(pairing.issued_count(), 2);

    pairing
        .revoke_all()
        .await
        .expect("revoke-all writes through");

    let after = Pairing::durable(registry(&store));
    assert_eq!(after.load().await.expect("reads"), 0);
    assert!(after.device_for(&one).is_none());
    assert!(after.device_for(&two).is_none());
}

#[tokio::test]
async fn a_signed_in_employee_survives_a_restart_inside_the_window() {
    let store = FakeStore::new();
    let alice = EmployeeId::new(Ulid::from_u128(1));
    let signed_in_at = at(10_000_000);
    let window = std::time::Duration::from_secs(30 * 60);

    // The device has to be paired first: the registry cascades a sign-in to its device, so a
    // session with no device is not representable — which is the invariant the contract suite pins.
    let pairing = Pairing::durable(registry(&store));
    let token = pair(&pairing, signed_in_at).await;
    let paired_device = pairing.device_for(&token).expect("the paired device");

    let sessions = Sessions::durable(registry(&store), window);
    sessions
        .sign_in(paired_device, alice, signed_in_at)
        .await
        .expect("records durably");

    // Restart, still inside the window.
    let after = Sessions::durable(registry(&store), window);
    let inside = at(signed_in_at.as_milliseconds_since_epoch() + 60_000);
    assert_eq!(after.load(inside).await.expect("reads"), 1);
    assert_eq!(
        after.employee_for(paired_device, inside),
        Some(alice),
        "a power blip does not make the shift re-enter its PIN"
    );

    // Restart again, this time past the window: the binding is not restored.
    let later = Sessions::durable(registry(&store), window);
    let outside = at(signed_in_at.as_milliseconds_since_epoch() + 31 * 60 * 1_000);
    assert_eq!(
        later.load(outside).await.expect("reads"),
        0,
        "a device idle past the window comes back signed out"
    );
    assert_eq!(later.employee_for(paired_device, outside), None);
}
