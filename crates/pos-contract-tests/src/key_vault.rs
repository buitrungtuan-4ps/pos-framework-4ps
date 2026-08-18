// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `KeyVault` suite.
//!
//! [`wipes_every_secret_a_machine_holds`] is the case that makes machine replacement safe:
//! [ADR-0003](../../../docs/adr/0003-cattle-not-pets.md) revokes the old machine to read-only, and
//! a credential left behind on it is a machine that can still talk to the cloud.

use pos_ports::PortName;
use pos_ports::key_vault::{KeyVault, Secret, SecretName};

use crate::harness::KeyVaultHarness;
use crate::{CaseFailure, Obligation};

/// Emits every `KeyVault` case as a `#[test]`.
#[macro_export]
macro_rules! key_vault_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_KEY_VAULT,
            module = key_vault,
            cases = [
                round_trips_a_secret,
                reports_an_unstored_secret_as_none,
                replaces_on_a_second_store,
                keeps_secrets_separate_by_name,
                deletes_idempotently,
                wipes_every_secret_a_machine_holds,
            ]
        }
    };
}

fn obligation() -> Obligation {
    Obligation::new(PortName::KeyVault, "store, load, delete, and never a file")
}

/// What goes in comes out.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn round_trips_a_secret<H: KeyVaultHarness>(harness: &H) -> Result<(), CaseFailure> {
    let vault = harness.fresh().await?;
    // Bytes, not text: a TPM-sealed credential is not UTF-8, and an adapter that assumes it is
    // corrupts one on the way through.
    let secret = Secret::new((0..=255_u8).collect());
    vault.store(SecretName::DeviceCredential, &secret).await?;
    let loaded = vault.load(SecretName::DeviceCredential).await?;
    let obligation = obligation();
    let loaded = obligation.require_nth(loaded.as_slice(), 0, "the stored secret")?;
    obligation.require_eq(
        &loaded.expose(),
        &secret.expose(),
        "the secret round-trips intact",
    )
}

/// First boot has nothing, and that is normal.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn reports_an_unstored_secret_as_none<H: KeyVaultHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let vault = harness.fresh().await?;
    obligation().require(
        vault.load(SecretName::DeviceCredential).await?.is_none(),
        "a machine that has never been activated holds no credential, and asking is not an error",
    )
}

/// Re-activation overwrites without deleting first.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn replaces_on_a_second_store<H: KeyVaultHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let vault = harness.fresh().await?;
    vault
        .store(SecretName::LeaseToken, &Secret::new(b"first".to_vec()))
        .await?;
    vault
        .store(SecretName::LeaseToken, &Secret::new(b"second".to_vec()))
        .await?;
    let loaded = vault.load(SecretName::LeaseToken).await?;
    let obligation = obligation();
    let loaded = obligation.require_nth(loaded.as_slice(), 0, "the stored lease")?;
    obligation.require_eq(
        &loaded.expose(),
        &b"second".as_slice(),
        "a lease renewal replaces the old token — it should not have to delete it first",
    )
}

/// Two names are two secrets.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn keeps_secrets_separate_by_name<H: KeyVaultHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let vault = harness.fresh().await?;
    let obligation = obligation();
    for (index, name) in SecretName::ALL.iter().enumerate() {
        let body = vec![u8::try_from(index).unwrap_or(0); 8];
        vault.store(*name, &Secret::new(body)).await?;
    }
    for (index, name) in SecretName::ALL.iter().enumerate() {
        let loaded = vault.load(*name).await?;
        let loaded = obligation.require_nth(loaded.as_slice(), 0, name.as_label())?;
        let expected = vec![u8::try_from(index).unwrap_or(0); 8];
        obligation.require_eq(
            &loaded.expose(),
            &expected.as_slice(),
            &format!("{name} kept its own value rather than another secret's"),
        )?;
    }
    Ok(())
}

/// Revocation runs more than once.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn deletes_idempotently<H: KeyVaultHarness>(harness: &H) -> Result<(), CaseFailure> {
    let vault = harness.fresh().await?;
    vault
        .store(SecretName::VendorApiKey, &Secret::new(b"key".to_vec()))
        .await?;
    vault.delete(SecretName::VendorApiKey).await?;
    vault.delete(SecretName::VendorApiKey).await?;
    obligation().require(
        vault.load(SecretName::VendorApiKey).await?.is_none(),
        "a deleted secret is gone, and deleting it again succeeds — revocation is retried",
    )
}

/// Everything a machine holds can be removed.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn wipes_every_secret_a_machine_holds<H: KeyVaultHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let vault = harness.fresh().await?;
    for name in SecretName::ALL {
        vault.store(*name, &Secret::new(b"body".to_vec())).await?;
    }
    for name in SecretName::ALL {
        vault.delete(*name).await?;
    }

    let obligation = obligation();
    for name in SecretName::ALL {
        obligation.require(
            vault.load(*name).await?.is_none(),
            format!(
                "{name} survived a wipe. Machine replacement revokes the old machine to \
                 read-only, and a credential left behind is a machine that can still talk to the \
                 cloud"
            ),
        )?;
    }
    Ok(())
}
