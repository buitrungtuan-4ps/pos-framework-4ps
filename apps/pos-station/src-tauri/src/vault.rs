// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The device token in the operating system's credential store (ADR-0147, ADR-0111 §"The token goes
//! where the operating system will keep it").
//!
//! One entry per edge: service `pos-station`, account the edge's origin (`http://192.168.1.10:8080`).
//! The backends are the ones the workspace's `key-vault-keyring` adapter chose (ADR-0086): Windows
//! Credential Manager, the macOS Keychain, and the Linux kernel keyring.
//!
//! # Why not the workspace's `KeyVault` port
//!
//! ADR-0111 sketched a shell implementing `pos_ports::key_vault::KeyVault` with a new `SecretName`
//! variant. ADR-0147 put the app outside the workspace, so it cannot depend on `pos-ports` without
//! taking the workspace's tree back in, and a new variant is a port change that needs its own ADR.
//! The same crate, the same backends and the same "absent is `None`, a broken store is an error"
//! rule, without the port.
//!
//! # The Linux limit
//!
//! The kernel keyring is memory: an entry survives a reinstall and a logout but **not a reboot**, so
//! a Linux terminal pairs again after one. Windows Credential Manager persists. A persistent Linux
//! store means the Secret Service over D-Bus, which ADR-0086 rejected for a headless edge and which a
//! desktop session may or may not run; that choice is left open in the guide.

/// The service every entry is filed under.
const SERVICE: &str = "pos-station";

/// The credential store could not be read or written.
#[derive(Debug)]
pub(crate) struct VaultError(String);

impl core::fmt::Display for VaultError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "the credential store refused: {}", self.0)
    }
}

fn entry(origin: &str) -> Result<keyring::Entry, VaultError> {
    keyring::Entry::new(SERVICE, origin).map_err(|error| VaultError(error.to_string()))
}

/// Stores the token issued by the edge at `origin`, replacing any earlier one.
pub(crate) fn save(origin: &str, token: &str) -> Result<(), VaultError> {
    entry(origin)?
        .set_password(token)
        .map_err(|error| VaultError(error.to_string()))
}

/// The token for `origin`, or `None` if this device holds none.
pub(crate) fn load(origin: &str) -> Result<Option<String>, VaultError> {
    match entry(origin)?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(VaultError(error.to_string())),
    }
}

/// Forgets the token for `origin`. Forgetting one that is not there succeeds.
pub(crate) fn forget(origin: &str) -> Result<(), VaultError> {
    match entry(origin)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(VaultError(error.to_string())),
    }
}
