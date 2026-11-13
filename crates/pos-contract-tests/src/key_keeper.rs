// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `KeyKeeper` suite
//! ([ADR-0217](../../../docs/adr/0217-secrets-are-sealed-under-versioned-tenant-keys.md)).
//!
//! The environment key wraps every tenant key, so a keeper that returned a wrong key instead of an
//! error, or wrapped a key in clear, would undo the whole envelope. Every adapter (the file keeper,
//! the `systemd-creds` keeper, a managed KMS) runs these cases.

use pos_ports::PortName;
use pos_ports::key_keeper::{KeyKeeper, WrappedKey};
use pos_ports::key_vault::Secret;
use pos_proto::ErrorStatus;

use crate::harness::KeyKeeperHarness;
use crate::{CaseFailure, Obligation};

/// Emits every `KeyKeeper` case as a `#[test]`.
#[macro_export]
macro_rules! key_keeper_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_KEY_KEEPER,
            module = key_keeper,
            cases = [
                round_trips_a_key,
                never_wraps_a_key_in_clear,
                refuses_a_tampered_key,
                refuses_another_keepers_key,
                names_a_version_it_does_not_hold,
            ]
        }
    };
}

fn obligation() -> Obligation {
    Obligation::new(PortName::KeyKeeper, "wrap, unwrap, and never a wrong key")
}

/// A tenant key: 32 bytes of every kind, never text.
fn tenant_key() -> Secret {
    Secret::new(
        (0_u8..32)
            .map(|byte| byte.wrapping_mul(37).wrapping_add(11))
            .collect(),
    )
}

/// Whether `haystack` holds `needle` anywhere.
fn holds(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// What goes in comes out.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn round_trips_a_key<H: KeyKeeperHarness>(harness: &H) -> Result<(), CaseFailure> {
    let keeper = harness.fresh().await?;
    let key = tenant_key();
    let wrapped = keeper.wrap(&key).await?;
    let opened = keeper.unwrap(&wrapped).await?;
    obligation().require(
        opened == key,
        "a wrapped tenant key unwraps to the same bytes",
    )
}

/// The wrapped bytes are not the key, and a second wrap is not the first.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn never_wraps_a_key_in_clear<H: KeyKeeperHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let keeper = harness.fresh().await?;
    let key = tenant_key();
    let first = keeper.wrap(&key).await?;
    let second = keeper.wrap(&key).await?;
    let obligation = obligation();
    obligation.require(
        !holds(&first.bytes, key.expose()),
        "a wrapped key holds the key in clear: a dump of the table would be the key",
    )?;
    obligation.require(
        first.bytes != second.bytes,
        "wrapping the same key twice gave the same bytes: the wrap reuses its nonce",
    )
}

/// One changed byte is an error, never a different key.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn refuses_a_tampered_key<H: KeyKeeperHarness>(harness: &H) -> Result<(), CaseFailure> {
    let keeper = harness.fresh().await?;
    let mut wrapped = keeper.wrap(&tenant_key()).await?;
    let obligation = obligation();
    let Some(byte) = wrapped.bytes.last_mut() else {
        return obligation.require(false, "a wrapped key has bytes");
    };
    *byte ^= 0x01;
    let outcome = keeper.unwrap(&wrapped).await;
    obligation.require(
        matches!(
            outcome.as_ref().map_err(pos_ports::PortError::status),
            Err(ErrorStatus::PermissionDenied | ErrorStatus::InvalidArgument)
        ),
        "a tampered wrapped key must be refused as such, never opened to some other key",
    )
}

/// A key another environment wrapped does not open here.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn refuses_another_keepers_key<H: KeyKeeperHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let keeper = harness.fresh().await?;
    let other = harness.other().await?;
    let wrapped = other.wrap(&tenant_key()).await?;
    let outcome = keeper.unwrap(&wrapped).await;
    obligation().require(
        matches!(
            outcome.as_ref().map_err(pos_ports::PortError::status),
            Err(ErrorStatus::PermissionDenied
                | ErrorStatus::InvalidArgument
                | ErrorStatus::NotFound)
        ),
        "a key another environment wrapped must not open here: environments never share a key",
    )
}

/// A version this keeper never held is named as missing.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn names_a_version_it_does_not_hold<H: KeyKeeperHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let keeper = harness.fresh().await?;
    let wrapped = keeper.wrap(&tenant_key()).await?;
    let unknown = WrappedKey {
        keeper_version: wrapped.keeper_version.saturating_add(1000),
        bytes: wrapped.bytes,
    };
    obligation().require_error(
        keeper.unwrap(&unknown).await,
        ErrorStatus::NotFound,
        "a version the keeper does not hold",
    )
}
