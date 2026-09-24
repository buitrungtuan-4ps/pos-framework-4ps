// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Secrets sealed on the machine's own disk under a key systemd unseals at every start
//! ([ADR-0151](../../../docs/adr/0151-a-headless-linux-box-seals-its-secrets-with-systemd-creds.md)).
//!
//! # Why
//!
//! The Linux backend keeps secrets in the kernel keyring, and the kernel keyring is empty after a
//! reboot. A headless store box that lost power therefore came back unactivated and needed a new
//! activation code (`docs/gate-register.md` row P2). The answer that row names is `systemd-creds`:
//! a secret encrypted with the machine's TPM2 when it has one, and otherwise with a host key only
//! root can read, which systemd decrypts again on the same machine.
//!
//! # How
//!
//! The service runs as the unprivileged `pos` user, and `systemd-creds` needs root. So the
//! installer seals one random **vault key**, once, as root. At every start a root helper
//! (`deploy/edge/pos-edge-vault unseal`, the unit's `ExecStartPre=-+`) decrypts it into a file the
//! service's group may read and only root may write. This module encrypts each secret with that key
//! (XChaCha20-Poly1305, the cipher the store's backups use) into `<dir>/<secret>.sealed`, and
//! decrypts it on load. No root process ever reads a file the service wrote.
//!
//! # Why a file here is not the file the port forbids
//!
//! [`KeyVault`](pos_ports::key_vault::KeyVault) contract §4 forbids writing a *secret* to a path,
//! and forbids falling back to a file when the protected store cannot be reached. A sealed file
//! holds ciphertext under a key only this machine's TPM2 or host key recovers; the secret is never
//! written. Nor is anything a fallback: with no vault key the edge uses the OS keyring as before,
//! and a sealed file that does not open is an error, not a reason to write somewhere else.
//!
//! # What it does not protect against
//!
//! Anyone who can run code as `pos` or root on the live machine can read the vault key, as they
//! could read the credential out of the process. With a TPM2 a copied disk is useless; without one
//! the host key is on the same disk, so a copied disk yields the credential. The credential is
//! scoped to one store and is revoked by archiving the device on the console.

use core::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chacha20poly1305::aead::{AeadInOut, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::Zeroize;

use crate::{BackendError, KeyringBackend, SERVICE};

/// The start of every sealed file: the format, so a stray file is refused by name rather than by a
/// failed decryption.
const MAGIC: &[u8; 4] = b"PEV1";

/// Bytes of nonce: XChaCha20-Poly1305's extended nonce, which is what makes a fresh random nonce
/// for every write safe.
const NONCE_LEN: usize = 24;

/// Bytes before the ciphertext.
const HEADER_LEN: usize = MAGIC.len() + NONCE_LEN;

/// The largest sealed file this store reads. A device credential is tens of bytes; the cap is what
/// stops a file that is not ours from being read into memory whole.
const MAX_SEALED_BYTES: u64 = 64 * 1024;

/// The file-name suffix of a sealed secret.
const SUFFIX: &str = "sealed";

/// Bytes in a vault key.
pub const VAULT_KEY_LEN: usize = 32;

/// The key sealed secrets are encrypted under: 32 random bytes that `systemd-creds` keeps sealed
/// and a root helper unseals for each start of the service.
pub struct VaultKey([u8; VAULT_KEY_LEN]);

impl VaultKey {
    /// A vault key from its raw bytes.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if `bytes` is not exactly [`VAULT_KEY_LEN`] long, which is
    /// what a truncated or wrong file looks like.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, BackendError> {
        <[u8; VAULT_KEY_LEN]>::try_from(bytes)
            .map(Self)
            .map_err(|_error| {
                BackendError::Unavailable(format!(
                    "a vault key is {VAULT_KEY_LEN} bytes and this one is {}",
                    bytes.len()
                ))
            })
    }

    /// Reads the vault key the start-up helper unsealed.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the file cannot be read or is not a vault key.
    pub fn read(path: &Path) -> Result<Self, BackendError> {
        let mut bytes = read_capped(path, VAULT_KEY_LEN as u64 + 1).map_err(|error| {
            BackendError::Unavailable(format!(
                "could not read the vault key at {}: {error}",
                path.display()
            ))
        })?;
        let key = Self::from_bytes(&bytes);
        bytes.zeroize();
        key
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new((&self.0).into())
    }
}

impl Drop for VaultKey {
    fn drop(&mut self) {
        // `zeroize` rather than `fill(0)`: a write to memory nothing reads again is exactly what
        // dead-store elimination removes. The same reasoning as the edge's `ArchiveKey`.
        self.0.zeroize();
    }
}

impl fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VaultKey(…)")
    }
}

/// A [`KeyringBackend`] that keeps each secret as `<dir>/<account>.sealed`, encrypted under a
/// [`VaultKey`].
#[derive(Debug, Clone)]
pub struct SealedFiles {
    dir: PathBuf,
    key: Arc<VaultKey>,
}

impl SealedFiles {
    /// Sealed secrets in `dir`, under `key`. The directory is created, private to the service,
    /// on the first write.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>, key: VaultKey) -> Self {
        Self {
            dir: dir.into(),
            key: Arc::new(key),
        }
    }

    /// Where the sealed secrets are.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, account: &str) -> PathBuf {
        self.dir.join(format!("{account}.{SUFFIX}"))
    }
}

/// Binds a sealed file to the secret it was written for, so a sealed device credential copied over
/// another secret's file does not open as that secret.
fn associated_data(account: &str) -> Vec<u8> {
    format!("{SERVICE}/{account}").into_bytes()
}

impl KeyringBackend for SealedFiles {
    fn set(&self, account: &str, secret: &[u8]) -> Result<(), BackendError> {
        let mut nonce = [0_u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|error| {
            BackendError::Unavailable(format!("the OS entropy source is unavailable: {error}"))
        })?;
        let mut body = secret.to_vec();
        let sealed = self.key.cipher().encrypt_in_place(
            &XNonce::from(nonce),
            &associated_data(account),
            &mut body,
        );
        if sealed.is_err() {
            body.zeroize();
            return Err(BackendError::Unavailable(format!(
                "the secret {account} could not be sealed"
            )));
        }
        let mut file = Vec::with_capacity(HEADER_LEN + body.len());
        file.extend_from_slice(MAGIC);
        file.extend_from_slice(&nonce);
        file.extend_from_slice(&body);
        create_private_dir(&self.dir)?;
        write_atomically(&self.path(account), &file)
    }

    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, BackendError> {
        let path = self.path(account);
        let file = match read_capped(&path, MAX_SEALED_BYTES) {
            Ok(file) => file,
            // Never stored, or deleted: the port's first-boot state, not a fault.
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(BackendError::Unavailable(format!(
                    "could not read {}: {error}",
                    path.display()
                )));
            }
        };
        let not_sealed = || {
            BackendError::Unavailable(format!(
                "{} is not a sealed secret this edge wrote",
                path.display()
            ))
        };
        let (header, body) = file.split_at_checked(HEADER_LEN).ok_or_else(not_sealed)?;
        let (magic, nonce) = header
            .split_at_checked(MAGIC.len())
            .ok_or_else(not_sealed)?;
        if magic != MAGIC {
            return Err(not_sealed());
        }
        let nonce = XNonce::try_from(nonce).map_err(|_error| not_sealed())?;
        let mut plain = body.to_vec();
        match self
            .key
            .cipher()
            .decrypt_in_place(&nonce, &associated_data(account), &mut plain)
        {
            Ok(()) => Ok(Some(plain)),
            Err(_error) => Err(BackendError::Unavailable(format!(
                "{} does not open with this machine's vault key: it was sealed under another key, \
                 or it has been altered",
                path.display()
            ))),
        }
    }

    fn delete(&self, account: &str) -> Result<(), BackendError> {
        match fs::remove_file(self.path(account)) {
            // Idempotent: revocation runs more than once (port contract §3).
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(BackendError::Unavailable(format!(
                "could not remove the sealed secret {account}: {error}"
            ))),
        }
    }
}

/// Reads at most `cap` bytes of `path`; a longer file is an error rather than a large allocation.
fn read_capped(path: &Path, cap: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(cap).read_to_end(&mut bytes)?;
    if bytes.len() as u64 >= cap {
        bytes.zeroize();
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("longer than the {cap} bytes a sealed secret or vault key can be"),
        ));
    }
    Ok(bytes)
}

/// Creates `dir` if it is missing, readable and writable by the service's user only.
fn create_private_dir(dir: &Path) -> Result<(), BackendError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir).map_err(|error| {
        BackendError::Unavailable(format!("could not create {}: {error}", dir.display()))
    })
}

/// Writes `bytes` to `path` so that a power cut leaves either the old file or the new one: a
/// private temporary file, flushed, then renamed over the old one, then the directory flushed so
/// the rename itself survives.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), BackendError> {
    let fail = |step: &str, error: io::Error| {
        BackendError::Unavailable(format!("could not {step} {}: {error}", path.display()))
    };
    let temporary = path.with_extension(format!("{SUFFIX}.tmp"));
    // A temporary file left by a write the power cut interrupted.
    match fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(fail("clear the temporary file beside", error)),
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| fail("write", error))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| fail("write", error))?;
    drop(file);
    fs::rename(&temporary, path).map_err(|error| fail("replace", error))?;
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        fs::File::open(dir)
            .and_then(|dir| dir.sync_all())
            .map_err(|error| fail("flush the directory of", error))?;
    }
    Ok(())
}

/// Sealed files first, and the keyring behind them only as the place a secret may still be from
/// before this box had a vault key.
///
/// A box activated before its installer sealed a vault key holds its credential in the kernel
/// keyring. The first read finds nothing sealed, finds it there, seals it and removes the keyring
/// copy, so the activation survives the next reboot without a new code. Every write goes to the
/// sealed store.
#[derive(Debug, Clone)]
pub struct SealedFirst<K> {
    sealed: SealedFiles,
    keyring: K,
}

impl<K> SealedFirst<K> {
    /// Sealed files in front of `keyring`.
    #[must_use]
    pub const fn new(sealed: SealedFiles, keyring: K) -> Self {
        Self { sealed, keyring }
    }

    /// Where the sealed secrets are.
    #[must_use]
    pub fn dir(&self) -> &Path {
        self.sealed.dir()
    }
}

impl<K: KeyringBackend> KeyringBackend for SealedFirst<K> {
    fn set(&self, account: &str, secret: &[u8]) -> Result<(), BackendError> {
        self.sealed.set(account, secret)?;
        // Best effort: an older copy in the keyring is only ever read when nothing is sealed, and
        // `delete` removes both, so a copy this misses cannot come back as the secret.
        let _stale = self.keyring.delete(account);
        Ok(())
    }

    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, BackendError> {
        if let Some(secret) = self.sealed.get(account)? {
            return Ok(Some(secret));
        }
        match self.keyring.get(account) {
            Ok(Some(mut secret)) => {
                let moved = self.sealed.set(account, &secret);
                if let Err(error) = moved {
                    secret.zeroize();
                    return Err(error);
                }
                let _moved = self.keyring.delete(account);
                Ok(Some(secret))
            }
            // A keyring this process cannot read has nothing to hand over; the sealed store is the
            // record now, and it has answered.
            Ok(None) | Err(_) => Ok(None),
        }
    }

    fn delete(&self, account: &str) -> Result<(), BackendError> {
        self.sealed.delete(account)?;
        match self.keyring.delete(account) {
            Ok(()) => Ok(()),
            // The keyring copy has to go too, or the next read would bring a revoked secret back.
            // A keyring that cannot be read cannot bring anything back, so only a copy that is
            // still readable makes the delete fail.
            Err(error) => match self.keyring.get(account) {
                Ok(Some(_still_there)) => Err(error),
                Ok(None) | Err(_) => Ok(()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HEADER_LEN, KeyringBackend, MAGIC, SealedFiles, SealedFirst, VAULT_KEY_LEN, VaultKey,
    };
    use crate::MemoryBackend;

    fn key(byte: u8) -> VaultKey {
        VaultKey::from_bytes(&[byte; VAULT_KEY_LEN]).expect("a 32-byte key")
    }

    fn sealed_in(dir: &tempfile::TempDir, byte: u8) -> SealedFiles {
        SealedFiles::new(dir.path().join("vault"), key(byte))
    }

    #[test]
    fn a_sealed_secret_opens_again_and_is_not_on_disk_in_clear() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let store = sealed_in(&dir, 7);
        let secret = b"device-credential-0123456789";
        store.set("device_credential", secret).expect("sealed");
        assert_eq!(
            store.get("device_credential").expect("opened"),
            Some(secret.to_vec())
        );
        let on_disk =
            std::fs::read(dir.path().join("vault/device_credential.sealed")).expect("on disk");
        assert!(on_disk.starts_with(MAGIC));
        assert!(
            !on_disk.windows(secret.len()).any(|window| window == secret),
            "the secret must not appear in the sealed file"
        );
    }

    #[test]
    fn a_secret_never_sealed_is_absent_and_a_delete_of_it_succeeds() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let store = sealed_in(&dir, 7);
        assert_eq!(store.get("device_credential").expect("read"), None);
        store.delete("device_credential").expect("idempotent");
        store.set("device_credential", b"x").expect("sealed");
        store.delete("device_credential").expect("deleted");
        store.delete("device_credential").expect("deleted twice");
        assert_eq!(store.get("device_credential").expect("read"), None);
    }

    #[test]
    fn a_file_sealed_under_another_key_is_an_error_not_an_absence() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        sealed_in(&dir, 7)
            .set("device_credential", b"secret")
            .expect("sealed");
        // A machine whose vault key changed (a TPM cleared, a disk moved to another box) must say
        // so. Reading it as "never activated" would hide the reason the box stopped syncing.
        assert!(sealed_in(&dir, 8).get("device_credential").is_err());
    }

    #[test]
    fn a_sealed_file_is_bound_to_the_secret_it_was_written_for() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let store = sealed_in(&dir, 7);
        store.set("device_credential", b"secret").expect("sealed");
        std::fs::copy(
            dir.path().join("vault/device_credential.sealed"),
            dir.path().join("vault/sync_key.sealed"),
        )
        .expect("copied");
        assert!(store.get("sync_key").is_err());
    }

    #[test]
    fn an_altered_or_foreign_file_does_not_open() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let store = sealed_in(&dir, 7);
        store.set("device_credential", b"secret").expect("sealed");
        let path = dir.path().join("vault/device_credential.sealed");
        let mut bytes = std::fs::read(&path).expect("on disk");
        if let Some(last) = bytes.last_mut() {
            *last ^= 1;
        }
        std::fs::write(&path, &bytes).expect("altered");
        assert!(store.get("device_credential").is_err());

        std::fs::write(&path, b"not a sealed file at all, and longer than a header")
            .expect("foreign");
        assert!(store.get("device_credential").is_err());
        std::fs::write(&path, &MAGIC[..]).expect("short");
        assert!(store.get("device_credential").is_err());
        assert!(HEADER_LEN > MAGIC.len());
    }

    #[test]
    fn a_second_write_replaces_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let store = sealed_in(&dir, 7);
        store.set("device_credential", b"first").expect("sealed");
        store.set("device_credential", b"second").expect("replaced");
        assert_eq!(
            store.get("device_credential").expect("opened"),
            Some(b"second".to_vec())
        );
        let names: Vec<String> = std::fs::read_dir(dir.path().join("vault"))
            .expect("listed")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["device_credential.sealed".to_owned()]);
    }

    #[cfg(unix)]
    #[test]
    fn the_directory_and_the_files_are_private_to_the_service() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a temporary directory");
        let store = sealed_in(&dir, 7);
        store.set("device_credential", b"secret").expect("sealed");
        let mode = |path: std::path::PathBuf| {
            std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
        };
        assert_eq!(mode(dir.path().join("vault")), 0o700);
        assert_eq!(
            mode(dir.path().join("vault/device_credential.sealed")),
            0o600
        );
    }

    #[test]
    fn a_vault_key_of_the_wrong_length_is_refused() {
        assert!(VaultKey::from_bytes(&[0; 31]).is_err());
        assert!(VaultKey::from_bytes(&[0; 33]).is_err());
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("vault-key");
        std::fs::write(&path, [0_u8; 64]).expect("written");
        assert!(VaultKey::read(&path).is_err());
        std::fs::write(&path, [0_u8; VAULT_KEY_LEN]).expect("written");
        assert!(VaultKey::read(&path).is_ok());
        assert_eq!(format!("{:?}", key(9)), "VaultKey(…)");
    }

    #[test]
    fn a_secret_left_in_the_keyring_moves_into_the_sealed_store_on_first_read() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let keyring = MemoryBackend::new();
        keyring
            .set("device_credential", b"from before the vault key")
            .expect("in the keyring");
        let vault = SealedFirst::new(sealed_in(&dir, 7), keyring);
        assert_eq!(
            vault.get("device_credential").expect("read"),
            Some(b"from before the vault key".to_vec())
        );
        // Now sealed, and gone from the keyring: the next reboot empties the keyring anyway, and a
        // copy left there would outlive a revocation that only reached the sealed store.
        assert_eq!(
            sealed_in(&dir, 7).get("device_credential").expect("read"),
            Some(b"from before the vault key".to_vec())
        );
        assert_eq!(vault.keyring.get("device_credential").expect("read"), None);
    }

    #[test]
    fn a_delete_reaches_both_stores_so_a_revoked_secret_does_not_come_back() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let keyring = MemoryBackend::new();
        let vault = SealedFirst::new(sealed_in(&dir, 7), keyring);
        vault.set("device_credential", b"sealed").expect("sealed");
        vault
            .keyring
            .set("device_credential", b"a stale copy")
            .expect("in the keyring");
        vault.delete("device_credential").expect("deleted");
        assert_eq!(vault.get("device_credential").expect("read"), None);
    }

    #[test]
    fn a_write_goes_to_the_sealed_store_and_clears_the_keyring() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let keyring = MemoryBackend::new();
        keyring
            .set("device_credential", b"old")
            .expect("in the keyring");
        let vault = SealedFirst::new(sealed_in(&dir, 7), keyring);
        vault.set("device_credential", b"new").expect("sealed");
        assert_eq!(vault.keyring.get("device_credential").expect("read"), None);
        assert_eq!(
            vault.get("device_credential").expect("read"),
            Some(b"new".to_vec())
        );
    }
}
