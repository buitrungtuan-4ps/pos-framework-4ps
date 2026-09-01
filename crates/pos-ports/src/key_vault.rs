// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Where a machine keeps its own credentials.
//!
//! `docs/architecture.md` §4 and [ADR-0003](../../../docs/adr/0003-cattle-not-pets.md) set
//! the flow: a machine is activated once with a short code, exchanges it for long-lived
//! credentials, and stores those in the operating system's own protected store — DPAPI or a
//! TPM on Windows, the keyring on Linux. The activation code is then useless, which is what
//! makes replacing a machine a five-minute job rather than a credential-distribution
//! exercise.
//!
//! # Why this one is asynchronous when [`crate::Signer`] is not
//!
//! Because it is genuinely I/O. DPAPI, a TPM and a keyring are syscalls into another
//! process or another chip; they block, they fail for reasons that have nothing to do with
//! the request, and on a locked keyring they can prompt. Verification is arithmetic; this
//! is a conversation.
//!
//! # Never a file, and never a log line
//!
//! `.gitignore` blocks `*.key` and `.env`, and `AGENTS.md` §8 says an agent never needs
//! production credentials. This port exists so that "store it somewhere" has exactly one
//! answer, and so the answer is never "next to the binary".

use core::fmt;

use core::future::Future;

use crate::error::PortError;

/// Names a stored secret.
///
/// A closed set rather than a string, so a typo cannot silently create a second secret that
/// nothing ever reads, and so the inventory of what a machine holds is enumerable —
/// [`Self::ALL`] is what a wipe-on-revocation routine iterates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum SecretName {
    /// The credential a store server presents to the cloud, obtained by exchanging an
    /// activation code.
    DeviceCredential,
    /// The current single-active lease. Held so a restart does not look like a second
    /// machine claiming the same store.
    LeaseToken,
    /// The key that signs webhook deliveries from this deployment.
    WebhookSigningKey,
    /// The per-tenant API key a cloud deployment uses against a vendor.
    VendorApiKey,
    /// The tenant-scoped `read_config` API key the edge presents to the cloud's `/sync` surface
    /// (config-pull and heartbeat, ADR-0085). Held in the vault so it need not live in a service-unit
    /// environment file; the edge falls back to `POS_EDGE_SYNC_KEY` for a headless bring-up (ADR-0086).
    SyncKey,
}

impl SecretName {
    /// Every secret a machine may hold.
    ///
    /// Iterated by the revocation path: when a lease is revoked the old machine goes
    /// read-only and its credentials are removed, and "remove everything" needs a list of
    /// what everything is.
    pub const ALL: &'static [Self] = &[
        Self::DeviceCredential,
        Self::LeaseToken,
        Self::WebhookSigningKey,
        Self::VendorApiKey,
        Self::SyncKey,
    ];

    /// The `snake_case` name under which the secret is stored.
    #[must_use]
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::DeviceCredential => "device_credential",
            Self::LeaseToken => "lease_token",
            Self::WebhookSigningKey => "webhook_signing_key",
            Self::VendorApiKey => "vendor_api_key",
            Self::SyncKey => "sync_key",
        }
    }
}

impl fmt::Display for SecretName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_label())
    }
}

/// Secret bytes.
///
/// # What this type does and does not guarantee
///
/// It guarantees that [`Debug`] and [`Display`](fmt::Display) never reveal the contents, so
/// a secret cannot reach a log through the ordinary route — a `{:?}` in a `tracing` field or
/// an error chain.
///
/// It does **not** guarantee erasure from memory. [`Drop`] overwrites the buffer, which
/// stops the bytes lingering in a freed allocation that is later handed to something else,
/// but the compiler is entitled to elide a write to memory that is about to be released, and
/// nothing here stops a copy having been made by [`Vec`] on a reallocation. A real guarantee
/// needs `zeroize`, which is a dependency this crate's allow-list does not carry and which
/// would need an ADR ([ADR-0007](../../../docs/adr/0007-in-house-vs-dependency.md)). The
/// distinction is written down so nobody reads `Drop` below and concludes more than it says.
pub struct Secret(Vec<u8>);

impl Secret {
    /// Wraps secret bytes.
    #[must_use]
    pub const fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The secret, for the one call that needs it.
    ///
    /// Deliberately named to be conspicuous in a diff. There is no `Deref`, no `AsRef` and
    /// no `Into<Vec<u8>>`, so every place a secret is used says so.
    #[must_use]
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// How many bytes, which is safe to log and occasionally useful.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the secret is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret({} bytes, redacted)", self.0.len())
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Best-effort overwrite. See the type's documentation for what this does not promise.
impl Drop for Secret {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// Constant-time comparison.
///
/// Comparing a presented credential against a stored one with `==` on a `Vec<u8>` leaks the
/// length of the matching prefix through timing, which is enough to recover a secret one
/// byte at a time. This is the one comparison in the framework where that matters.
impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        if self.0.len() != other.0.len() {
            return false;
        }
        let mut difference = 0_u8;
        for (left, right) in self.0.iter().zip(other.0.iter()) {
            difference |= left ^ right;
        }
        difference == 0
    }
}

impl Eq for Secret {}

/// Stores a machine's own credentials in the operating system's protected store.
///
/// # Contract
///
/// 1. **`load` of an absent secret is `Ok(None)`.** A machine that has never been activated
///    is the normal first-boot state, not a fault.
/// 2. **`store` replaces.** Re-activation and lease renewal both overwrite, and neither
///    should have to delete first.
/// 3. **`delete` of an absent secret succeeds**, because revocation runs more than once and
///    must be safe to re-run.
/// 4. **Nothing here ever writes a secret to a path under the binary.** An adapter that
///    cannot reach a protected store fails with [`PortError::unavailable`]; it does not fall
///    back to a file. A silent fallback is how a credential ends up in a backup.
pub trait KeyVault: Send + Sync {
    /// Stores or replaces a secret.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the protected store cannot be reached — a locked
    /// keyring, an absent TPM — or [`PortError::permission_denied`] if the process is not
    /// entitled to write it.
    fn store(
        &self,
        name: SecretName,
        secret: &Secret,
    ) -> impl Future<Output = Result<(), PortError>> + Send;

    /// Reads a secret, or `None` if it was never stored.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the protected store cannot be reached, or
    /// [`PortError::permission_denied`] if the process is not entitled to read it.
    fn load(
        &self,
        name: SecretName,
    ) -> impl Future<Output = Result<Option<Secret>, PortError>> + Send;

    /// Removes a secret, succeeding whether or not it was there.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the protected store cannot be reached.
    fn delete(&self, name: SecretName) -> impl Future<Output = Result<(), PortError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{Secret, SecretName};

    #[test]
    fn a_secret_cannot_reach_a_log_through_debug_or_display() {
        let secret = Secret::new(b"hunter2-and-then-some".to_vec());
        let debugged = format!("{secret:?}");
        let displayed = format!("{secret}");
        assert!(!debugged.contains("hunter2"), "got {debugged}");
        assert!(!displayed.contains("hunter2"), "got {displayed}");
        assert!(
            debugged.contains("21 bytes"),
            "the length is safe and useful"
        );
    }

    #[test]
    fn comparison_does_not_short_circuit_on_the_first_differing_byte() {
        // Testing timing itself is not practical here; what is testable is that the
        // implementation folds every byte rather than returning early, and that equality
        // still behaves.
        let stored = Secret::new(vec![1, 2, 3, 4]);
        assert_eq!(stored, Secret::new(vec![1, 2, 3, 4]));
        assert_ne!(stored, Secret::new(vec![1, 2, 3, 5]));
        assert_ne!(stored, Secret::new(vec![9, 2, 3, 4]));
        assert_ne!(stored, Secret::new(vec![1, 2, 3]));
    }

    #[test]
    fn every_secret_name_is_enumerable_so_revocation_can_wipe_them_all() {
        // The revocation path in ADR-0003 needs a list, not a guess about what a machine
        // might be holding.
        assert_eq!(SecretName::ALL.len(), 5);
        let mut labels: Vec<&str> = SecretName::ALL.iter().map(|name| name.as_label()).collect();
        let count = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), count, "two secrets share a storage name");
    }

    #[test]
    fn an_empty_secret_is_representable_and_says_so() {
        let empty = Secret::new(Vec::new());
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
    }
}
