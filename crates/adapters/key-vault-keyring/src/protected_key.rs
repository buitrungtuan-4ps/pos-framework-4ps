//! A vault key kept on disk under the operating system's own protection, created on first use
//! ([ADR-0174](../../../docs/adr/0174-a-windows-store-keeps-its-state-to-its-own-service.md)
//! decision 3).
//!
//! On Linux a root helper unseals the vault key with `systemd-creds` before each start (ADR-0151),
//! and the service reads it as raw bytes. A Windows service has no such helper, so it keeps the key
//! itself: protected with DPAPI in machine scope, in a file in `data\`. The first start that finds
//! no file makes a random key, protects it, writes it, and reads it back to prove it opens before
//! any secret is sealed under it.
//!
//! The protection is a seam, [`KeyProtection`], so the whole sequence is tested on Linux with a
//! stand-in; [`Dpapi`] is the Windows one.

use std::io;
use std::path::Path;

use zeroize::Zeroize;

use crate::BackendError;
use crate::sealed::{VAULT_KEY_LEN, VaultKey, create_private_dir, read_capped, write_atomically};

/// The most a protected vault key file may hold. A DPAPI blob around 32 bytes is a few hundred.
const PROTECTED_CAP: u64 = 4096;

/// What protects the vault key at rest, and opens it again.
pub trait KeyProtection {
    /// Protects `plain`.
    ///
    /// # Errors
    ///
    /// [`BackendError`] if the protection refuses.
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, BackendError>;

    /// Opens what [`KeyProtection::protect`] made.
    ///
    /// # Errors
    ///
    /// [`BackendError`] if it was not made here, or is damaged.
    fn unprotect(&self, protected: &[u8]) -> Result<Vec<u8>, BackendError>;
}

/// Opens the vault key at `path`, or, where there is none yet, makes one, keeps it there and proves
/// it opens.
///
/// # Errors
///
/// [`BackendError::Unavailable`] if the file cannot be read or written, does not open, or opens to
/// something that is not a vault key. A file that does not open is never replaced: the secrets
/// sealed under it would be lost with it.
pub fn open_or_create(
    path: &Path,
    protection: &impl KeyProtection,
) -> Result<VaultKey, BackendError> {
    match read_capped(path, PROTECTED_CAP) {
        Ok(protected) => open(&protected, protection),
        Err(error) if error.kind() == io::ErrorKind::NotFound => create(path, protection),
        Err(error) => Err(BackendError::Unavailable(format!(
            "could not read the vault key at {}: {error}",
            path.display()
        ))),
    }
}

fn open(protected: &[u8], protection: &impl KeyProtection) -> Result<VaultKey, BackendError> {
    let mut plain = protection.unprotect(protected)?;
    let key = VaultKey::from_bytes(&plain);
    plain.zeroize();
    key
}

fn create(path: &Path, protection: &impl KeyProtection) -> Result<VaultKey, BackendError> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let mut plain = [0_u8; VAULT_KEY_LEN];
    getrandom::fill(&mut plain).map_err(|error| {
        BackendError::Unavailable(format!("no randomness for a new vault key: {error}"))
    })?;
    let written = protection
        .protect(&plain)
        .and_then(|protected| write_atomically(path, &protected));
    let proven = written.and_then(|()| {
        let back = read_capped(path, PROTECTED_CAP).map_err(|error| {
            BackendError::Unavailable(format!("could not read back {}: {error}", path.display()))
        })?;
        let mut opened = protection.unprotect(&back)?;
        let same = opened == plain;
        opened.zeroize();
        if same {
            Ok(())
        } else {
            Err(BackendError::Unavailable(format!(
                "the new vault key at {} did not open as it was made",
                path.display()
            )))
        }
    });
    let key = proven.and_then(|()| VaultKey::from_bytes(&plain));
    plain.zeroize();
    key
}

/// DPAPI in machine scope, through `windows-protect`.
#[cfg(windows)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Dpapi;

#[cfg(windows)]
impl KeyProtection for Dpapi {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, BackendError> {
        windows_protect::protect_for_machine(plain)
            .map_err(|error| BackendError::Unavailable(error.to_string()))
    }

    fn unprotect(&self, protected: &[u8]) -> Result<Vec<u8>, BackendError> {
        windows_protect::unprotect(protected)
            .map_err(|error| BackendError::Unavailable(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{KeyProtection, open_or_create};
    use crate::BackendError;
    use crate::sealed::VAULT_KEY_LEN;

    /// A stand-in for DPAPI: XOR with a byte, and a marker so a foreign file does not open.
    struct Xor {
        protected: AtomicUsize,
    }

    impl Xor {
        const fn new() -> Self {
            Self {
                protected: AtomicUsize::new(0),
            }
        }
    }

    impl KeyProtection for Xor {
        fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, BackendError> {
            self.protected.fetch_add(1, Ordering::SeqCst);
            let mut out = b"XOR:".to_vec();
            out.extend(plain.iter().map(|byte| byte ^ 0x5A));
            Ok(out)
        }

        fn unprotect(&self, protected: &[u8]) -> Result<Vec<u8>, BackendError> {
            protected
                .strip_prefix(b"XOR:")
                .map(|body| body.iter().map(|byte| byte ^ 0x5A).collect())
                .ok_or_else(|| BackendError::Unavailable("not protected here".to_owned()))
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("protected-key-{name}-{}", std::process::id()));
        let _fresh = std::fs::remove_dir_all(&dir);
        dir.join("data").join("vault.key")
    }

    #[test]
    fn the_first_start_makes_a_key_and_the_next_opens_the_same_one() {
        let path = scratch("first");
        let protection = Xor::new();
        let made = open_or_create(&path, &protection).expect("made");
        let raw = std::fs::read(&path).expect("kept");
        assert!(raw.starts_with(b"XOR:"), "kept protected, never raw");
        assert_eq!(raw.len(), 4 + VAULT_KEY_LEN);
        let opened = open_or_create(&path, &protection).expect("opened");
        assert_eq!(made.bytes(), opened.bytes());
        assert_eq!(
            protection.protected.load(Ordering::SeqCst),
            1,
            "made once, opened after"
        );
    }

    #[test]
    fn a_key_file_that_does_not_open_is_an_error_and_is_never_replaced() {
        let path = scratch("foreign");
        std::fs::create_dir_all(path.parent().expect("dir")).expect("dir");
        std::fs::write(&path, b"from another machine").expect("write");
        assert!(open_or_create(&path, &Xor::new()).is_err());
        assert_eq!(std::fs::read(&path).expect("kept"), b"from another machine");
    }

    #[test]
    fn a_key_that_opens_to_the_wrong_length_is_refused() {
        let path = scratch("short");
        std::fs::create_dir_all(path.parent().expect("dir")).expect("dir");
        let protection = Xor::new();
        std::fs::write(&path, protection.protect(&[1, 2, 3]).expect("protect")).expect("write");
        assert!(open_or_create(&path, &protection).is_err());
    }
}
