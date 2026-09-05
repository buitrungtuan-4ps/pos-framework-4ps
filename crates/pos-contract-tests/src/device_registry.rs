// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `DeviceRegistry` suite ([ADR-0091](../../../docs/adr/0091-durable-edge-auth-state.md)).
//!
//! Two cases here are the ones that matter most, and both are about a *disagreement* between the
//! two tables rather than about either one working:
//!
//! * [`revoking_a_device_clears_its_sign_in`] — a session belonging to no paired device is
//!   unreachable state that a later feature could read as live. Revocation has to reach both.
//! * [`a_revoked_token_stops_resolving`] — the whole point of making revocation explicit. Before
//!   this port a restart revoked everything by accident; an implementation that stores but never
//!   forgets would pass every other case here and still be unusable.
//!
//! The idle timeout is deliberately **not** tested here: the port stores `last_seen_at` and the
//! caller decides expiry, so there is no rule for an adapter to get wrong. What is tested is that
//! [`touch_session`](pos_ports::DeviceRegistry::touch_session) actually moves the stored instant,
//! because a timeout reading a value nothing updates would expire every session on schedule
//! regardless of use.

use pos_ports::PortName;
use pos_ports::device_registry::{DeviceRegistry, DeviceSession, PairedDevice, TokenDigest};
use pos_proto::ids::{DeviceId, EmployeeId};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;

use crate::harness::DeviceRegistryHarness;
use crate::{CaseFailure, Obligation};

/// Emits every `DeviceRegistry` case as a `#[test]`.
#[macro_export]
macro_rules! device_registry_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_DEVICE_REGISTRY,
            module = device_registry,
            cases = [
                resolves_a_token_to_the_device_it_was_issued_to,
                reports_an_unknown_token_as_none,
                lists_every_paired_device,
                a_revoked_token_stops_resolving,
                revoking_a_device_clears_its_sign_in,
                revoking_is_idempotent,
                revoke_all_retires_every_device,
                round_trips_a_sign_in,
                a_second_sign_in_replaces_the_first,
                touch_moves_last_seen_forward,
                touching_a_device_with_no_session_is_not_an_error,
                clearing_a_sign_in_is_idempotent,
                two_devices_keep_separate_sign_ins,
            ]
        }
    };
}

fn obligation() -> Obligation {
    Obligation::new(
        PortName::DeviceRegistry,
        "pairing and sign-in survive a restart, and revocation reaches both tables",
    )
}

/// A device id built from a small number, so a case can name two distinct devices readably.
fn device(n: u128) -> DeviceId {
    DeviceId::new(Ulid::from_u128(n))
}

/// An employee id, likewise.
fn employee(n: u128) -> EmployeeId {
    EmployeeId::new(Ulid::from_u128(n))
}

/// A digest that differs per `n`. Not a real SHA-256 of anything — the port takes a digest as an
/// opaque value, so what matters is only that two of them differ.
fn digest(n: u8) -> TokenDigest {
    TokenDigest::from_bytes([n; 32])
}

/// An instant, in milliseconds since the epoch.
fn at(ms: i64) -> Timestamp {
    Timestamp::from_milliseconds_since_epoch(ms).unwrap_or(Timestamp::EPOCH)
}

/// A paired device record.
fn paired(n: u128, digest_byte: u8, ms: i64) -> PairedDevice {
    PairedDevice {
        device_id: device(n),
        token_digest: digest(digest_byte),
        paired_at: at(ms),
    }
}

/// A sign-in record.
fn session(device_n: u128, employee_n: u128, ms: i64) -> DeviceSession {
    DeviceSession {
        device_id: device(device_n),
        employee_id: employee(employee_n),
        signed_in_at: at(ms),
        last_seen_at: at(ms),
    }
}

/// A digest in, the device it belongs to out.
///
/// This is the obligation, not a description of the shipped request path: `pos-edge` loads the
/// whole table at boot and resolves from memory (production-readiness **X2**). An adapter still
/// has to answer the point read correctly — it is how this suite proves `record_pairing` stored
/// the binding at all.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn resolves_a_token_to_the_device_it_was_issued_to<H: DeviceRegistryHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let registry = harness.fresh().await?;
    registry.record_pairing(paired(1, 0xa1, 1_000)).await?;
    let resolved = registry.device_for_token(digest(0xa1)).await?;
    let obligation = obligation();
    let resolved = obligation.require_nth(resolved.as_slice(), 0, "the paired device")?;
    obligation.require_eq(
        resolved,
        &device(1),
        "the digest resolves to the device it was issued to",
    )
}

/// A token nothing issued must not resolve — and asking is not an error.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn reports_an_unknown_token_as_none<H: DeviceRegistryHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let registry = harness.fresh().await?;
    registry.record_pairing(paired(1, 0xa1, 1_000)).await?;
    obligation().require(
        registry.device_for_token(digest(0xff)).await?.is_none(),
        "a digest that was never issued resolves to nothing, and looking it up is not a fault",
    )
}

/// The pairing screen and the boot path both need the list.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn lists_every_paired_device<H: DeviceRegistryHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let registry = harness.fresh().await?;
    registry.record_pairing(paired(1, 0xa1, 1_000)).await?;
    registry.record_pairing(paired(2, 0xa2, 2_000)).await?;
    let mut listed: Vec<DeviceId> = registry
        .paired_devices()
        .await?
        .into_iter()
        .map(|device| device.device_id)
        .collect();
    listed.sort_unstable();
    obligation().require_eq(
        &listed,
        &vec![device(1), device(2)],
        "every admitted device is listed, in no particular order",
    )
}

/// Explicit revocation is what replaces the restart that used to revoke by accident.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn a_revoked_token_stops_resolving<H: DeviceRegistryHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let registry = harness.fresh().await?;
    registry.record_pairing(paired(1, 0xa1, 1_000)).await?;
    registry.revoke_device(device(1)).await?;
    let obligation = obligation();
    obligation.require(
        registry.device_for_token(digest(0xa1)).await?.is_none(),
        "a revoked device's token authenticates nothing",
    )?;
    obligation.require(
        registry.paired_devices().await?.is_empty(),
        "and it is gone from the list, not merely unresolvable",
    )
}

/// The two tables must not be left disagreeing.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn revoking_a_device_clears_its_sign_in<H: DeviceRegistryHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let registry = harness.fresh().await?;
    registry.record_pairing(paired(1, 0xa1, 1_000)).await?;
    registry.record_sign_in(session(1, 7, 1_500)).await?;
    registry.revoke_device(device(1)).await?;
    let obligation = obligation();
    obligation.require(
        registry.sign_in_for(device(1)).await?.is_none(),
        "a session belonging to no paired device is unreachable state a later feature could read \
         as live, so revocation clears it",
    )?;
    obligation.require(
        registry.sign_ins().await?.is_empty(),
        "and it is gone from the sign-in list too",
    )
}

/// An operator who is unsure it worked runs it again.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn revoking_is_idempotent<H: DeviceRegistryHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let registry = harness.fresh().await?;
    registry.record_pairing(paired(1, 0xa1, 1_000)).await?;
    registry.revoke_device(device(1)).await?;
    registry.revoke_device(device(1)).await?;
    // A device that was never there at all, too — an operator can mistype an id.
    registry.revoke_device(device(99)).await?;
    obligation().require(
        registry.paired_devices().await?.is_empty(),
        "revoking twice, and revoking something absent, both succeed",
    )
}

/// The break-glass: reproduce, on purpose, what a restart used to do by accident.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn revoke_all_retires_every_device<H: DeviceRegistryHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let registry = harness.fresh().await?;
    registry.record_pairing(paired(1, 0xa1, 1_000)).await?;
    registry.record_pairing(paired(2, 0xa2, 2_000)).await?;
    registry.record_sign_in(session(1, 7, 1_500)).await?;
    registry.revoke_all_devices().await?;
    let obligation = obligation();
    obligation.require(
        registry.paired_devices().await?.is_empty(),
        "no device survives a revoke-all",
    )?;
    obligation.require(
        registry.sign_ins().await?.is_empty(),
        "and no sign-in outlives the device it was on",
    )?;
    // Idempotent, like the single-device revoke.
    registry.revoke_all_devices().await?;
    obligation.require(
        registry.paired_devices().await?.is_empty(),
        "a second revoke-all is a no-op, not an error",
    )
}

/// What the durable sign-in is for: the person is still there after a restart.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn round_trips_a_sign_in<H: DeviceRegistryHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let registry = harness.fresh().await?;
    registry.record_pairing(paired(1, 0xa1, 1_000)).await?;
    let recorded = session(1, 7, 1_500);
    registry.record_sign_in(recorded).await?;
    let loaded = registry.sign_in_for(device(1)).await?;
    let obligation = obligation();
    let loaded = obligation.require_nth(loaded.as_slice(), 0, "the recorded sign-in")?;
    obligation.require_eq(
        loaded,
        &recorded,
        "every field round-trips, including both instants — the timeout reads one of them",
    )
}

/// One employee per device: a second sign-in replaces rather than adds.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn a_second_sign_in_replaces_the_first<H: DeviceRegistryHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let registry = harness.fresh().await?;
    registry.record_pairing(paired(1, 0xa1, 1_000)).await?;
    registry.record_sign_in(session(1, 7, 1_500)).await?;
    registry.record_sign_in(session(1, 8, 2_500)).await?;
    let obligation = obligation();
    let loaded = registry.sign_in_for(device(1)).await?;
    let loaded = obligation.require_nth(loaded.as_slice(), 0, "the current sign-in")?;
    obligation.require_eq(
        &loaded.employee_id,
        &employee(8),
        "the shift handover replaces the person, it does not stack them",
    )?;
    obligation.require_eq(
        &registry.sign_ins().await?.len(),
        &1_usize,
        "and there is exactly one row for the device",
    )
}

/// The timeout reads `last_seen_at`, so something has to move it.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn touch_moves_last_seen_forward<H: DeviceRegistryHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let registry = harness.fresh().await?;
    registry.record_pairing(paired(1, 0xa1, 1_000)).await?;
    registry.record_sign_in(session(1, 7, 1_500)).await?;
    registry.touch_session(device(1), at(9_000)).await?;
    let obligation = obligation();
    let loaded = registry.sign_in_for(device(1)).await?;
    let loaded = obligation.require_nth(loaded.as_slice(), 0, "the touched sign-in")?;
    obligation.require_eq(
        &loaded.last_seen_at,
        &at(9_000),
        "an idle timeout reading a value nothing updates would expire every session on schedule \
         regardless of use",
    )?;
    obligation.require_eq(
        &loaded.signed_in_at,
        &at(1_500),
        "a touch is not a fresh sign-in, so when they signed in is unchanged",
    )
}

/// The gate touches on every request, including one racing a sign-out.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn touching_a_device_with_no_session_is_not_an_error<H: DeviceRegistryHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let registry = harness.fresh().await?;
    registry.record_pairing(paired(1, 0xa1, 1_000)).await?;
    registry.touch_session(device(1), at(9_000)).await?;
    // And on a device the registry has never heard of.
    registry.touch_session(device(99), at(9_000)).await?;
    obligation().require(
        registry.sign_in_for(device(1)).await?.is_none(),
        "touching creates nothing — a touch is not a way to sign a device in",
    )
}

/// Signing out runs again when staff are unsure it took.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn clearing_a_sign_in_is_idempotent<H: DeviceRegistryHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let registry = harness.fresh().await?;
    registry.record_pairing(paired(1, 0xa1, 1_000)).await?;
    registry.record_sign_in(session(1, 7, 1_500)).await?;
    registry.clear_sign_in(device(1)).await?;
    registry.clear_sign_in(device(1)).await?;
    let obligation = obligation();
    obligation.require(
        registry.sign_in_for(device(1)).await?.is_none(),
        "the device is signed out and clearing again is a no-op",
    )?;
    obligation.require(
        !registry.paired_devices().await?.is_empty(),
        "signing out does not un-pair the device — it stays admitted, with nobody on it",
    )
}

/// Two tills, two people, no bleed.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn two_devices_keep_separate_sign_ins<H: DeviceRegistryHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let registry = harness.fresh().await?;
    registry.record_pairing(paired(1, 0xa1, 1_000)).await?;
    registry.record_pairing(paired(2, 0xa2, 1_000)).await?;
    registry.record_sign_in(session(1, 7, 1_500)).await?;
    registry.record_sign_in(session(2, 8, 1_500)).await?;
    let obligation = obligation();
    let first = registry.sign_in_for(device(1)).await?;
    let first = obligation.require_nth(first.as_slice(), 0, "the first device's sign-in")?;
    let second = registry.sign_in_for(device(2)).await?;
    let second = obligation.require_nth(second.as_slice(), 0, "the second device's sign-in")?;
    obligation.require_eq(&first.employee_id, &employee(7), "the first till's person")?;
    obligation.require_eq(
        &second.employee_id,
        &employee(8),
        "the second till's person",
    )?;

    // And signing one out leaves the other alone — the case that catches a table keyed by
    // something other than the device.
    registry.clear_sign_in(device(1)).await?;
    obligation.require(
        registry.sign_in_for(device(2)).await?.is_some(),
        "signing one till out must not sign the other out",
    )
}
