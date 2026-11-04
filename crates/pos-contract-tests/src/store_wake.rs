// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `StoreWake` suite
//! ([ADR-0195](../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md)).
//!
//! The obligations with teeth are [`connected_comes_first_and_after_every_reconnect`], because a
//! wake is not durable and the pull on connecting is the only catch-up a store gets for the ones
//! it missed, and [`hears_only_its_own_stores_wakes`], because a store pulling on another's wake
//! is a fleet pulling on every publish.

use pos_ports::PortName;
use pos_ports::store_wake::{StoreWake, WakeSignal};
use pos_proto::ids::ConfigVersionId;
use pos_proto::ulid::Ulid;
use pos_proto::wake::ConfigWake;

use crate::harness::StoreWakeHarness;
use crate::{CaseFailure, Obligation};

/// Emits every `StoreWake` case as a `#[test]`.
#[macro_export]
macro_rules! store_wake_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_STORE_WAKE,
            module = store_wake,
            cases = [
                connected_comes_first_and_after_every_reconnect,
                a_wake_keeps_its_version,
                hears_only_its_own_stores_wakes,
                an_idle_wait_ends_with_nothing,
            ]
        }
    };
}

fn wake(n: u128) -> ConfigWake {
    ConfigWake {
        config_version_id: ConfigVersionId::new(Ulid::from_u128(n)),
    }
}

/// Subscribes for `store` and takes the first signal, which must be [`WakeSignal::Connected`].
async fn connected<H: StoreWakeHarness>(
    harness: &H,
    store: pos_proto::ids::StoreId,
) -> Result<H::Wake, CaseFailure> {
    let subscribed = harness.subscribe(store).await?;
    let first = subscribed.next_signal(harness.patience()).await?;
    Obligation::new(PortName::StoreWake, "connected comes first").require_eq(
        &first,
        &Some(WakeSignal::Connected),
        "the first signal after subscribing, so a store pulls once on connecting",
    )?;
    Ok(subscribed)
}

/// The first signal is `Connected`, and a reconnect reports it again.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn connected_comes_first_and_after_every_reconnect<H: StoreWakeHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let (store, _) = harness.store_ids();
    let subscribed = connected(harness, store).await?;
    harness.reconnect(&subscribed).await?;
    let again = subscribed.next_signal(harness.patience()).await?;
    Obligation::new(PortName::StoreWake, "connected comes first").require_eq(
        &again,
        &Some(WakeSignal::Connected),
        "a reconnect is reported, so a store that was away pulls the wakes it missed",
    )
}

/// The wake heard is the wake sent.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn a_wake_keeps_its_version<H: StoreWakeHarness>(harness: &H) -> Result<(), CaseFailure> {
    let (store, _) = harness.store_ids();
    let subscribed = connected(harness, store).await?;
    harness.send(store, &wake(41)).await?;
    let heard = subscribed.next_signal(harness.patience()).await?;
    Obligation::new(PortName::StoreWake, "a wake keeps its version").require_eq(
        &heard,
        &Some(WakeSignal::Wake(Some(wake(41)))),
        "the version names what to skip; a store holding it does nothing",
    )
}

/// A wake sent to another store is not heard; one sent here after it is.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn hears_only_its_own_stores_wakes<H: StoreWakeHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let (store, other) = harness.store_ids();
    let subscribed = connected(harness, store).await?;
    harness.send(other, &wake(7)).await?;
    harness.send(store, &wake(8)).await?;
    let heard = subscribed.next_signal(harness.patience()).await?;
    Obligation::new(PortName::StoreWake, "only this store's wakes").require_eq(
        &heard,
        &Some(WakeSignal::Wake(Some(wake(8)))),
        "another store's wake is never reported here",
    )
}

/// With nothing sent, the wait ends with `None` rather than holding the caller.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn an_idle_wait_ends_with_nothing<H: StoreWakeHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let (store, _) = harness.store_ids();
    let subscribed = connected(harness, store).await?;
    let heard = subscribed.next_signal(harness.patience()).await?;
    Obligation::new(PortName::StoreWake, "waiting is bounded").require_eq(
        &heard,
        &None,
        "nothing heard within the wait is `None`, so the caller's poll keeps its schedule",
    )
}
