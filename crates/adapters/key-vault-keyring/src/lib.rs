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
//! The `linux-native` keyutils path (and Credential Manager) is a fast local syscall, so
//! [`KeyringVault`] performs it inline and returns a ready future.
//!
//! # A reboot empties the kernel keyring, so a Linux box seals its secrets
//!
//! [`OsVault`] is what the edge runs on. Given a vault key that `systemd-creds` kept and a root
//! helper unsealed for this start, it keeps each secret sealed on disk ([`sealed`],
//! [ADR-0151](../../../docs/adr/0151-a-headless-linux-box-seals-its-secrets-with-systemd-creds.md)),
//! so a headless box's activation survives a power cut; that is file I/O, so it runs on the blocking
//! pool. Given no key, it is the OS keyring as above.

#![forbid(unsafe_code)]

use core::future::{Future, ready};

use pos_ports::PortName;
use pos_ports::error::PortError;
use pos_ports::key_vault::{KeyVault, Secret, SecretName};

pub mod sealed;

pub use sealed::{SealedFiles, SealedFirst, VAULT_KEY_LEN, VaultKey};

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
// The vault the edge runs on: sealed secrets when the service was given a vault key, the OS keyring
// otherwise (ADR-0151).
// -------------------------------------------------------------------------------------------------

/// The environment variable naming the vault key the start-up helper unsealed for this run
/// (`deploy/edge/pos-edge.service.d/vault.conf` sets it).
pub const VAULT_KEY_ENV: &str = "POS_EDGE_VAULT_KEY";

/// The environment variable naming the directory sealed secrets are kept in.
pub const VAULT_DIR_ENV: &str = "POS_EDGE_VAULT_DIR";

/// Where the edge's secrets live this run, for the start-up log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultStanding {
    /// In the OS keyring, because the service was not given a vault key. On Linux that is the
    /// kernel keyring, which a reboot empties.
    Keyring,
    /// Sealed under the vault key systemd unsealed for this start, in this directory.
    Sealed(std::path::PathBuf),
    /// A vault key was configured and could not be used, for the reason given. Every vault call
    /// fails with it.
    Unusable(String),
}

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
#[derive(Debug, Clone)]
enum OsBackend {
    Keyring(OsKeyring),
    Sealed(SealedFirst<OsKeyring>),
    Unusable(String),
}

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
impl KeyringBackend for OsBackend {
    fn set(&self, account: &str, secret: &[u8]) -> Result<(), BackendError> {
        match self {
            Self::Keyring(keyring) => keyring.set(account, secret),
            Self::Sealed(sealed) => sealed.set(account, secret),
            Self::Unusable(reason) => Err(BackendError::Unavailable(reason.clone())),
        }
    }

    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, BackendError> {
        match self {
            Self::Keyring(keyring) => keyring.get(account),
            Self::Sealed(sealed) => sealed.get(account),
            Self::Unusable(reason) => Err(BackendError::Unavailable(reason.clone())),
        }
    }

    fn delete(&self, account: &str) -> Result<(), BackendError> {
        match self {
            Self::Keyring(keyring) => keyring.delete(account),
            Self::Sealed(sealed) => sealed.delete(account),
            Self::Unusable(reason) => Err(BackendError::Unavailable(reason.clone())),
        }
    }
}

/// The edge's [`KeyVault`]: sealed secrets when the service was given a vault key, the OS keyring
/// when it was not.
///
/// # A key that cannot be used is an error, never the keyring
///
/// Falling back would read an activated box as never activated, and the till sends an unactivated
/// box's counter to `/setup`. A vault that answers an error instead leaves the counter trading and
/// pauses cloud sync until the key is fixed, which is the degraded mode ADR-0001 asks for.
///
/// # Off the async workers
///
/// Sealed secrets are files, so every call runs on the blocking pool (`AGENTS.md` §2): this needs a
/// Tokio runtime, which is what the edge runs on.
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
#[derive(Debug, Clone)]
pub struct OsVault {
    backend: OsBackend,
    standing: VaultStanding,
}

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
impl OsVault {
    /// The vault the service's environment describes: [`VAULT_KEY_ENV`] and [`VAULT_DIR_ENV`].
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_paths(
            std::env::var_os(VAULT_KEY_ENV).map(std::path::PathBuf::from),
            std::env::var_os(VAULT_DIR_ENV).map(std::path::PathBuf::from),
        )
    }

    /// The vault for an unsealed key file and a directory: sealed secrets when both are given and
    /// the key reads, an unusable vault when it does not or when only one is given, and the OS
    /// keyring when neither is.
    #[must_use]
    pub fn from_paths(key: Option<std::path::PathBuf>, dir: Option<std::path::PathBuf>) -> Self {
        let (key, dir) = match (key, dir) {
            (Some(key), Some(dir)) => (key, dir),
            (None, None) => {
                return Self {
                    backend: OsBackend::Keyring(OsKeyring::new()),
                    standing: VaultStanding::Keyring,
                };
            }
            (Some(_), None) | (None, Some(_)) => {
                return Self::unusable(format!(
                    "{VAULT_KEY_ENV} and {VAULT_DIR_ENV} are set together or not at all"
                ));
            }
        };
        match VaultKey::read(&key) {
            Ok(vault_key) => Self {
                backend: OsBackend::Sealed(SealedFirst::new(
                    SealedFiles::new(dir.clone(), vault_key),
                    OsKeyring::new(),
                )),
                standing: VaultStanding::Sealed(dir),
            },
            Err(error) => Self::unusable(error.to_string()),
        }
    }

    fn unusable(reason: String) -> Self {
        Self {
            backend: OsBackend::Unusable(reason.clone()),
            standing: VaultStanding::Unusable(reason),
        }
    }

    /// Where this vault keeps secrets.
    #[must_use]
    pub const fn standing(&self) -> &VaultStanding {
        &self.standing
    }
}

/// Runs one vault operation on the blocking pool and maps its failure to the port's error.
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
async fn off_the_workers<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, BackendError> + Send + 'static,
) -> Result<T, PortError> {
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result.map_err(to_port_error),
        Err(error) => Err(PortError::unavailable(
            PortName::KeyVault,
            "the vault operation did not finish",
        )
        .with_source(error)),
    }
}

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
impl KeyVault for OsVault {
    fn store(
        &self,
        name: SecretName,
        secret: &Secret,
    ) -> impl Future<Output = Result<(), PortError>> + Send {
        let backend = self.backend.clone();
        let secret = Secret::new(secret.expose().to_vec());
        off_the_workers(move || backend.set(name.as_label(), secret.expose()))
    }

    fn load(
        &self,
        name: SecretName,
    ) -> impl Future<Output = Result<Option<Secret>, PortError>> + Send {
        let backend = self.backend.clone();
        off_the_workers(move || {
            backend
                .get(name.as_label())
                .map(|bytes| bytes.map(Secret::new))
        })
    }

    fn delete(&self, name: SecretName) -> impl Future<Output = Result<(), PortError>> + Send {
        let backend = self.backend.clone();
        off_the_workers(move || backend.delete(name.as_label()))
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

#[cfg(all(
    test,
    any(target_os = "linux", target_os = "windows", target_os = "macos")
))]
mod os_vault {
    use super::{OsVault, VAULT_KEY_LEN, VaultStanding};
    use pos_contract_tests::harness::{KeyVaultHarness, Setup};
    use pos_ports::key_vault::{KeyVault, Secret, SecretName};
    use std::sync::Mutex;

    /// Drives a future on a current-thread runtime: `OsVault` runs each call on Tokio's blocking
    /// pool, which even a current-thread runtime has.
    #[expect(
        clippy::expect_used,
        reason = "test scaffolding: a runtime that cannot be built is a broken test host"
    )]
    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime")
            .block_on(future)
    }

    /// A sealed vault per case, each in its own directory, kept until the harness is dropped.
    #[derive(Default)]
    struct SealedVaultHarness {
        dirs: Mutex<Vec<tempfile::TempDir>>,
    }

    impl KeyVaultHarness for SealedVaultHarness {
        type Vault = OsVault;

        #[expect(
            clippy::expect_used,
            reason = "test scaffolding: a temporary directory that cannot be made is a broken host"
        )]
        async fn fresh(&self) -> Setup<Self::Vault> {
            let dir = tempfile::tempdir().expect("a temporary directory");
            let key = dir.path().join("vault-key");
            std::fs::write(&key, [5_u8; VAULT_KEY_LEN]).expect("a vault key");
            let vault = OsVault::from_paths(Some(key), Some(dir.path().join("vault")));
            assert!(matches!(vault.standing(), VaultStanding::Sealed(_)));
            self.dirs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(dir);
            Ok(vault)
        }
    }

    // The sealed store meets the same contract the keyring does, through the vault the edge uses.
    pos_contract_tests::key_vault_suite!(SealedVaultHarness::default(), block_on);

    #[test]
    fn no_vault_key_is_the_os_keyring_as_before() {
        assert_eq!(
            OsVault::from_paths(None, None).standing(),
            &VaultStanding::Keyring
        );
    }

    #[test]
    fn a_vault_key_that_does_not_read_makes_every_call_fail_rather_than_fall_back() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let missing = OsVault::from_paths(
            Some(dir.path().join("no-such-key")),
            Some(dir.path().join("vault")),
        );
        assert!(matches!(missing.standing(), VaultStanding::Unusable(_)));
        // An error, so the counter keeps trading; `Ok(None)` would read as "never activated" and
        // send the till to /setup.
        assert!(block_on(missing.load(SecretName::DeviceCredential)).is_err());
        assert!(
            block_on(missing.store(SecretName::DeviceCredential, &Secret::new(vec![1]))).is_err()
        );
        assert!(block_on(missing.delete(SecretName::DeviceCredential)).is_err());

        let short = dir.path().join("short-key");
        std::fs::write(&short, [1_u8; 8]).expect("a short key");
        assert!(matches!(
            OsVault::from_paths(Some(short), Some(dir.path().join("vault"))).standing(),
            VaultStanding::Unusable(_)
        ));
    }

    #[test]
    fn a_key_without_a_directory_or_the_reverse_is_a_mistake_not_the_keyring() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        assert!(matches!(
            OsVault::from_paths(Some(dir.path().join("vault-key")), None).standing(),
            VaultStanding::Unusable(_)
        ));
        assert!(matches!(
            OsVault::from_paths(None, Some(dir.path().join("vault"))).standing(),
            VaultStanding::Unusable(_)
        ));
    }

    #[test]
    fn a_sealed_secret_outlives_the_process_that_stored_it() {
        // The whole point: a second OsVault over the same key and directory, as after a reboot,
        // reads what the first stored.
        let dir = tempfile::tempdir().expect("a temporary directory");
        let key = dir.path().join("vault-key");
        std::fs::write(&key, [5_u8; VAULT_KEY_LEN]).expect("a vault key");
        let before = OsVault::from_paths(Some(key.clone()), Some(dir.path().join("vault")));
        block_on(before.store(
            SecretName::DeviceCredential,
            &Secret::new(b"credential".to_vec()),
        ))
        .expect("stored");
        drop(before);
        let after = OsVault::from_paths(Some(key), Some(dir.path().join("vault")));
        let loaded = block_on(after.load(SecretName::DeviceCredential)).expect("loaded");
        assert_eq!(
            loaded.map(|secret| secret.expose().to_vec()),
            Some(b"credential".to_vec())
        );
    }
}
