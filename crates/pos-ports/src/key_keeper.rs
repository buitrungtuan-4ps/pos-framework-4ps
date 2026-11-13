// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Where a cloud's environment key is kept
//! ([ADR-0217](../../../docs/adr/0217-secrets-are-sealed-under-versioned-tenant-keys.md)).
//!
//! The cloud seals each tenant's secrets under that tenant's own key, and stores the tenant key
//! only *wrapped* by the environment key. This port is the environment key: it wraps a tenant key
//! and unwraps it again, and does nothing else. The key itself never leaves the keeper, so a
//! deployment chooses where it lives (a file for development, `systemd-creds` sealed to the host's
//! TPM2, a managed KMS) by configuration, and the code that seals secrets does not change.
//!
//! # Versions
//!
//! Every environment key has a version, and every wrapped key names the version that wrapped it
//! ([`WrappedKey::keeper_version`]), so a keeper that has been rotated still unwraps what an older
//! version wrapped while it holds that version. A keeper wraps under its newest version.

use core::fmt;
use core::future::Future;

use crate::error::PortError;
use crate::key_vault::Secret;

/// A tenant key as the cloud stores it: wrapped by one version of the environment key.
#[derive(Clone, PartialEq, Eq)]
pub struct WrappedKey {
    /// The environment-key version that wrapped it. Safe to log.
    pub keeper_version: u32,
    /// The wrapped bytes: opaque, and never the key itself.
    pub bytes: Vec<u8>,
}

impl fmt::Debug for WrappedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "WrappedKey(v{}, {} bytes)",
            self.keeper_version,
            self.bytes.len()
        )
    }
}

/// Wraps and unwraps tenant keys under the environment key.
///
/// # Contract
///
/// 1. **What is wrapped unwraps intact**, whatever its bytes.
/// 2. **A wrapped key is never the key.** Its bytes do not contain the key, and wrapping the same
///    key twice gives different bytes.
/// 3. **Tampering is refused.** A wrapped key whose bytes were changed, or that another keeper
///    wrapped, does not unwrap: [`PortError::permission_denied`] or [`PortError::invalid_argument`], never a
///    wrong key.
/// 4. **A version the keeper does not hold is [`PortError::not_found`]**, so a rotation that retired
///    a version too early is named rather than mistaken for tampering.
/// 5. **Nothing here logs a key.** An error names a version, never bytes.
pub trait KeyKeeper: Send + Sync {
    /// Wraps `key` under the newest environment-key version.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the keeper cannot reach its key, or no entropy is available.
    fn wrap(&self, key: &Secret) -> impl Future<Output = Result<WrappedKey, PortError>> + Send;

    /// Unwraps a key [`Self::wrap`] wrapped, under the version it names.
    ///
    /// # Errors
    ///
    /// [`PortError::not_found`] if the keeper does not hold that version,
    /// [`PortError::permission_denied`] if the bytes do not open under it, or
    /// [`PortError::unavailable`] if the keeper cannot reach its key.
    fn unwrap(
        &self,
        wrapped: &WrappedKey,
    ) -> impl Future<Output = Result<Secret, PortError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::WrappedKey;

    #[test]
    fn a_wrapped_key_prints_its_version_and_length_only() {
        let wrapped = WrappedKey {
            keeper_version: 3,
            bytes: b"opaque-bytes".to_vec(),
        };
        let printed = format!("{wrapped:?}");
        assert_eq!(printed, "WrappedKey(v3, 12 bytes)");
    }
}
