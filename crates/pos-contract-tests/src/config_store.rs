// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `ConfigStore` suite.
//!
//! The obligation with the most operational weight is [`keeps_last_known_good_after_a_refusal`].
//! Configuration is cloud-owned ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md)),
//! so a bad version reaches every store at once — and the only thing standing between "somebody
//! published a typo" and "the fleet stops trading" is that a store which refuses a version keeps
//! running the last one that worked.

use pos_ports::config_store::{ConfigDelta, ConfigSnapshot, ConfigStore, ConfigUpdate};
use pos_ports::{PortName, Transactional, TxContext};
use pos_proto::{ConfigVersionId, ErrorStatus, StoreId, Ulid};

use crate::harness::ConfigStoreHarness;
use crate::{CaseFailure, Obligation, fixtures};

/// Emits every `ConfigStore` case as a `#[test]`.
#[macro_export]
macro_rules! config_store_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_CONFIG_STORE,
            module = config_store,
            cases = [
                starts_with_no_configuration,
                applies_a_snapshot_from_any_version,
                applies_a_delta_from_the_stated_version,
                refuses_a_delta_from_the_wrong_version,
                is_idempotent_on_a_repeated_update,
                keeps_last_known_good_after_a_refusal,
                keeps_configuration_across_power_loss,
            ]
        }
    };
}

fn ordering_rule() -> Obligation {
    Obligation::new(
        PortName::ConfigStore,
        "a delta applies only from its stated version",
    )
}

fn idempotency() -> Obligation {
    Obligation::new(
        PortName::ConfigStore,
        "applying the same update twice is a no-op",
    )
}

fn recovery() -> Obligation {
    Obligation::new(
        PortName::ConfigStore,
        "last-known-good survives a bad version",
    )
}

fn version(seed: u32) -> ConfigVersionId {
    ConfigVersionId::new(Ulid::from_u128(u128::from(seed)))
}

fn snapshot(store_id: StoreId, seed: u32, json: &str) -> ConfigUpdate {
    ConfigUpdate::Snapshot(ConfigSnapshot {
        config_version_id: version(seed),
        store_id,
        document: fixtures::config_document(json),
    })
}

fn delta(store_id: StoreId, from: u32, to: u32, json: &str) -> ConfigUpdate {
    ConfigUpdate::Delta(ConfigDelta {
        from_config_version_id: version(from),
        to_config_version_id: version(to),
        store_id,
        patch: fixtures::config_document(json),
    })
}

async fn apply_committed<S: ConfigStore>(
    store: &S,
    update: &ConfigUpdate,
) -> Result<ConfigVersionId, CaseFailure> {
    let mut tx = store.begin().await?;
    let applied = store.apply(&mut tx, update).await?;
    tx.commit().await?;
    Ok(applied)
}

/// Before first sync there is no configuration, and that is not an error.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn starts_with_no_configuration<H: ConfigStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let obligation = recovery();
    obligation.require(
        store.current(harness.store_id()).await?.is_none(),
        "a store that has never synchronised has no current version — first boot is a normal \
         state, not a fault",
    )?;
    obligation.require(
        store.last_known_good(harness.store_id()).await?.is_none(),
        "and no last-known-good either",
    )
}

/// A snapshot applies whatever the store is holding.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn applies_a_snapshot_from_any_version<H: ConfigStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    let obligation = ordering_rule();

    apply_committed(&store, &snapshot(store_id, 1, r#"{"tables_enabled":true}"#)).await?;

    // From version 1 straight to version 9, with nothing in between. That is the whole point of
    // a snapshot: it is the recovery path for a store too far behind to patch, so it must not
    // require the intervening versions to exist.
    let applied = apply_committed(
        &store,
        &snapshot(store_id, 9, r#"{"tables_enabled":false}"#),
    )
    .await?;
    obligation.require_eq(&applied, &version(9), "a snapshot reaches its own version")?;

    let current = store.current(store_id).await?;
    let current = obligation.require_nth(current.as_slice(), 0, "the current snapshot")?;
    obligation.require_eq(
        &current.config_version_id,
        &version(9),
        "the current version is the snapshot's",
    )?;
    obligation.require_eq(
        current.document.as_json(),
        r#"{"tables_enabled":false}"#,
        "a snapshot replaces the document rather than merging into it",
    )
}

/// A delta applies from the version it names.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn applies_a_delta_from_the_stated_version<H: ConfigStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    let obligation = ordering_rule();

    apply_committed(&store, &snapshot(store_id, 1, r#"{"tables_enabled":true}"#)).await?;
    let applied =
        apply_committed(&store, &delta(store_id, 1, 2, r#"{"tips_enabled":true}"#)).await?;
    obligation.require_eq(
        &applied,
        &version(2),
        "a delta reaches the version it names",
    )?;

    let current = store.current(store_id).await?;
    let current = obligation.require_nth(current.as_slice(), 0, "the current snapshot")?;
    obligation.require_eq(
        &current.config_version_id,
        &version(2),
        "the store now reports the delta's target version",
    )
}

/// A delta whose `from` is not the current version changes nothing.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn refuses_a_delta_from_the_wrong_version<H: ConfigStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    let obligation = ordering_rule();

    apply_committed(&store, &snapshot(store_id, 1, r#"{"tables_enabled":true}"#)).await?;

    let mut tx = store.begin().await?;
    let outcome = store
        .apply(&mut tx, &delta(store_id, 7, 8, r#"{"tips_enabled":true}"#))
        .await;
    obligation.require_error(
        outcome,
        ErrorStatus::FailedPrecondition,
        "a delta from a version the store is not holding must be refused. Configuration is a \
         tree, so an out-of-order patch produces a document nobody authored — and it would then \
         be reported as a version the cloud believes it published",
    )?;
    drop(tx);

    let current = store.current(store_id).await?;
    let current = obligation.require_nth(current.as_slice(), 0, "the current snapshot")?;
    obligation.require_eq(
        &current.config_version_id,
        &version(1),
        "a refused delta leaves the current version untouched",
    )
}

/// The cloud publishes at-least-once, so a repeat is expected traffic.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn is_idempotent_on_a_repeated_update<H: ConfigStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    let obligation = idempotency();

    let update = snapshot(store_id, 1, r#"{"tables_enabled":true}"#);
    apply_committed(&store, &update).await?;
    let again = apply_committed(&store, &update).await?;
    obligation.require_eq(
        &again,
        &version(1),
        "a repeated snapshot reports the same version",
    )?;

    let patch = delta(store_id, 1, 2, r#"{"tips_enabled":true}"#);
    apply_committed(&store, &patch).await?;

    // Called directly rather than through the helper, because this case needs the port's own
    // status rather than a wrapped failure: a repeated delta has two defensible answers and only
    // one indefensible one.
    let mut tx = store.begin().await?;
    let repeat = store.apply(&mut tx, &patch).await;
    let verdict = match repeat {
        Ok(reached) => obligation.require_eq(
            &reached,
            &version(2),
            "a repeated delta reports the version already reached",
        ),
        Err(error) => obligation.require(
            error.status() == ErrorStatus::FailedPrecondition,
            format!(
                "a repeated delta may be refused as a precondition — the store is no longer at \
                 its `from` version — but not with {}",
                pos_proto::wire_enum::WireEnum::as_wire(error.status())
            ),
        ),
    };
    drop(tx);
    verdict
}

/// After a refusal, the last version that worked is still available.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn keeps_last_known_good_after_a_refusal<H: ConfigStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    let obligation = recovery();

    apply_committed(&store, &snapshot(store_id, 1, r#"{"tables_enabled":true}"#)).await?;

    // A version the store cannot apply. Whether an adapter refuses it for shape or for content
    // is its business; what matters is what survives.
    let mut tx = store.begin().await?;
    let _ = store
        .apply(&mut tx, &delta(store_id, 99, 100, r#"{"broken":true}"#))
        .await;
    drop(tx);

    let good = store.last_known_good(store_id).await?;
    let good = obligation.require_nth(good.as_slice(), 0, "last-known-good")?;
    obligation.require_eq(
        &good.config_version_id,
        &version(1),
        "a store that refuses a version keeps running the last one that worked. Configuration is \
         cloud-owned, so a bad version reaches the whole fleet at once, and this is the only \
         thing between a published typo and every store stopping",
    )
}

/// Configuration is durable, because a store that forgets it cannot sell.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn keeps_configuration_across_power_loss<H: ConfigStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    apply_committed(&store, &snapshot(store_id, 1, r#"{"tables_enabled":true}"#)).await?;

    let store = harness.lose_power(store).await?;
    let obligation = recovery();
    let current = store.current(store_id).await?;
    let current = obligation.require_nth(current.as_slice(), 0, "the current snapshot")?;
    obligation.require_eq(
        &current.config_version_id,
        &version(1),
        "configuration survives a power cut. A store that came back with none would have no menu \
         and no prices, which is the one degraded state with no path that keeps selling",
    )
}
