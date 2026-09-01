// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge's [`KeyVault`] adapter: a machine's own credentials in the operating system's protected
//! store ([ADR-0086](../../../docs/adr/0086-edge-keyvault-and-activation.md),
//! [ADR-0003](../../../docs/adr/0003-cattle-not-pets.md)).
//!
//! A store server is activated once with a short code, exchanges it for a long-lived device
//! credential, and keeps that credential in the OS credential store — never a file beside the binary
//! ([`key_vault`](pos_ports::key_vault) contract §4). This is the field adapter behind that port,
//! over the maintained cross-platform [`keyring`] crate: **Windows Credential Manager**, the **macOS
//! Keychain**, and on **Linux the kernel keyring (keyutils)** via `keyring`'s `linux-native`
//! backend — no D-Bus / Secret Service daemon, so it works under the headless store service user
//! (ADR-0086 rejects Secret Service for exactly that reason).
//!
//! # The socket lives behind a seam
//!
//! [`KeyringBackend`] is the one thing that touches the OS store; [`KeyringVault`] is pure over it —
//! it maps a [`SecretName`] to an account within the service, wraps and unwraps a [`Secret`], and
//! translates a backend failure to a [`PortError`]. So the adapter's whole behaviour is proven
//! against the in-memory [`MemoryBackend`] in the fast pull-request gate (see the tests below), and
//! the real `keyring` integration is proven against a live OS store in the gated lane
//! (`tests/integration.rs`, `--features integration`) — the same split `store-postgres` and
//! `cloud-sync-http` draw.
//!
//! The port is async because a DPAPI/TPM/keyring call is genuinely I/O ([ADR-0026](../../../docs/adr/0026-port-shapes.md)).
//! The `linux-native` keyutils path (and Credential Manager) is a fast local syscall, so this adapter
//! performs it inline and returns a ready future; a heavier TPM-sealed backend that genuinely blocks
//! would offload to `spawn_blocking` behind this same seam (ADR-0086's flagged hardware follow-up).

#![forbid(unsafe_code)]

use core::future::{Future, ready};

use pos_ports::PortName;
use pos_ports::error::PortError;
use pos_ports::key_vault::{KeyVault, Secret, SecretName};

/// The service the edge's secrets are grouped under in the OS credential store. Each [`SecretName`]
/// is an account within it, so the store holds one named entry per secret a machine may carry.
const SERVICE: &str = "pizza4ps-pos-edge";

/// A failure of the underlying OS credential store, as distinct from an absent secret (which is
/// `Ok(None)`). Carries a human-readable reason for the store's log — a credential name is not a
/// secret, and the reason never contains the secret bytes.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// The store could not be reached — a locked keyring, an absent daemon, a platform fault.
    #[error("the credential store is unavailable: {0}")]
    Unavailable(String),
    /// The process is not entitled to read or write the entry.
    #[error("the process is not entitled to access the credential store: {0}")]
    Denied(String),
}

/// The seam over one OS credential-store entry, keyed by `account` within [`SERVICE`].
///
/// Synchronous: a keyutils / Credential Manager / Keychain call is a fast local syscall, not a
/// network round-trip, so [`KeyringVault`] calls it inline. `get` of an absent entry is `Ok(None)`
/// and `delete` of an absent entry is `Ok(())` — the port's never-a-fault contract for a first-boot
/// machine and a re-run revocation.
pub trait KeyringBackend: Send + Sync {
    /// Stores or replaces the entry's bytes.
    ///
    /// # Errors
    ///
    /// [`BackendError`] if the store is unreachable or the process is not entitled to write.
    fn set(&self, account: &str, secret: &[u8]) -> Result<(), BackendError>;

    /// Reads the entry's bytes, or `None` if it was never stored.
    ///
    /// # Errors
    ///
    /// [`BackendError`] if the store is unreachable or the process is not entitled to read.
    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, BackendError>;

    /// Removes the entry, succeeding whether or not it was there.
    ///
    /// # Errors
    ///
    /// [`BackendError`] if the store is unreachable.
    fn delete(&self, account: &str) -> Result<(), BackendError>;
}

/// The [`KeyVault`] implementation: pure mapping over a [`KeyringBackend`].
#[derive(Debug, Clone)]
pub struct KeyringVault<B> {
    backend: B,
}

impl<B: KeyringBackend> KeyringVault<B> {
    /// Builds a vault over `backend`.
    pub const fn new(backend: B) -> Self {
        Self { backend }
    }
}

/// Maps a backend failure to the port's error, naming the [`KeyVault`] port. Never carries secret
/// bytes — only the store's own diagnostic reason.
fn to_port_error(error: BackendError) -> PortError {
    match error {
        BackendError::Unavailable(message) => PortError::unavailable(PortName::KeyVault, message),
        BackendError::Denied(message) => PortError::permission_denied(PortName::KeyVault, message),
    }
}

impl<B: KeyringBackend> KeyVault for KeyringVault<B> {
    fn store(
        &self,
        name: SecretName,
        secret: &Secret,
    ) -> impl Future<Output = Result<(), PortError>> + Send {
        // A fast local syscall: perform it inline and hand back a ready future (see the module doc).
        ready(
            self.backend
                .set(name.as_label(), secret.expose())
                .map_err(to_port_error),
        )
    }

    fn load(
        &self,
        name: SecretName,
    ) -> impl Future<Output = Result<Option<Secret>, PortError>> + Send {
        ready(
            self.backend
                .get(name.as_label())
                .map(|bytes| bytes.map(Secret::new))
                .map_err(to_port_error),
        )
    }

    fn delete(&self, name: SecretName) -> impl Future<Output = Result<(), PortError>> + Send {
        ready(self.backend.delete(name.as_label()).map_err(to_port_error))
    }
}

// -------------------------------------------------------------------------------------------------
// The production backend: the OS credential store via the `keyring` crate.
// -------------------------------------------------------------------------------------------------

/// The production [`KeyringBackend`], over the OS credential store (`keyring` crate).
///
/// Compiled on the platforms the fleet targets — Linux (kernel keyutils), Windows (Credential
/// Manager), macOS (Keychain). On any other target the crate still builds as the seam plus the
/// in-memory backend, so a fork on an exotic platform can supply its own backend.
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
#[derive(Debug, Clone, Default)]
pub struct OsKeyring;

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
impl OsKeyring {
    /// Builds the OS-keyring backend.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// The `keyring` entry for one account within [`SERVICE`].
    fn entry(account: &str) -> Result<keyring::Entry, BackendError> {
        keyring::Entry::new(SERVICE, account).map_err(map_keyring_error)
    }
}

/// Maps a `keyring` error to a [`BackendError`]. `NoEntry` is handled by the callers (it is not a
/// fault), so here it collapses with the platform faults; `NoStorageAccess` is the entitlement case.
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "keyring::Error is #[non_exhaustive]: NoStorageAccess is the one entitlement case, and \
              every other present-or-future fault is a store we could not reach (unavailable). A \
              wildcard is required and is the safe default for a new upstream variant."
)]
fn map_keyring_error(error: keyring::Error) -> BackendError {
    match error {
        keyring::Error::NoStorageAccess(source) => BackendError::Denied(source.to_string()),
        other => BackendError::Unavailable(other.to_string()),
    }
}

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
impl KeyringBackend for OsKeyring {
    fn set(&self, account: &str, secret: &[u8]) -> Result<(), BackendError> {
        Self::entry(account)?
            .set_secret(secret)
            .map_err(map_keyring_error)
    }

    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, BackendError> {
        match Self::entry(account)?.get_secret() {
            Ok(bytes) => Ok(Some(bytes)),
            // Never activated / already wiped is the normal first-boot and post-revocation state.
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(other) => Err(map_keyring_error(other)),
        }
    }

    fn delete(&self, account: &str) -> Result<(), BackendError> {
        match Self::entry(account)?.delete_credential() {
            // Idempotent: revocation runs more than once (port contract §3).
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(other) => Err(map_keyring_error(other)),
        }
    }
}

// -------------------------------------------------------------------------------------------------
// The in-memory backend: proves the adapter's mapping in the fast gate, and backs unit tests.
// -------------------------------------------------------------------------------------------------

/// An in-memory [`KeyringBackend`] for tests: it exercises [`KeyringVault`]'s mapping and error
/// translation with no OS store, so the adapter's behaviour is checked in the pull-request gate. It
/// is not a `KeyVault` — that is the point; the real store is proven separately in the gated lane.
#[derive(Debug, Default)]
pub struct MemoryBackend {
    entries: std::sync::Mutex<std::collections::BTreeMap<String, Vec<u8>>>,
}

impl MemoryBackend {
    /// An empty backend.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, std::collections::BTreeMap<String, Vec<u8>>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl KeyringBackend for MemoryBackend {
    fn set(&self, account: &str, secret: &[u8]) -> Result<(), BackendError> {
        self.lock().insert(account.to_owned(), secret.to_vec());
        Ok(())
    }

    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, BackendError> {
        Ok(self.lock().get(account).cloned())
    }

    fn delete(&self, account: &str) -> Result<(), BackendError> {
        self.lock().remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{KeyringVault, MemoryBackend};
    use pos_contract_tests::harness::{KeyVaultHarness, Setup};
    use pos_fakes::executor::run_ready;

    /// Drives the shared `KeyVault` contract suite against the adapter over the in-memory backend.
    struct MemVaultHarness;

    impl KeyVaultHarness for MemVaultHarness {
        type Vault = KeyringVault<MemoryBackend>;

        async fn fresh(&self) -> Setup<Self::Vault> {
            Ok(KeyringVault::new(MemoryBackend::new()))
        }
    }

    // The adapter's mapping and semantics — round-trip (all 256 byte values), absent-is-none,
    // replace, per-name separation, idempotent delete, wipe-everything — proven with no OS store.
    pos_contract_tests::key_vault_suite!(MemVaultHarness, run_ready);
}
