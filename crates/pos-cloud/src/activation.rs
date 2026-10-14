// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud half of the once-only activation exchange (P9, [ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)).
//!
//! The pure rule — that a code is redeemed at most once — lives in
//! [`pos_core::activation`]; this module is the seam that lets the HTTP handlers act on it against a
//! store. A super-admin issues a code bound to a device slot; the device presents it and receives a
//! long-lived credential in exchange. The exchange must be atomic — redeem the code and mint the
//! credential together — so the seam exposes one [`ActivationCodeStore::consume_and_provision`]
//! method the adapter runs in a single transaction, rather than composing separate calls.
//!
//! Only hashes cross into storage: an activation code is kept as `SHA-256` of its canonical text
//! ([`hash_code`]), a credential as `SHA-256` of its secret, so a database dump leaks neither a live
//! code nor a usable credential. The credential is a `posdev_<id>_<secret>` bearer token, the
//! api-key shape ([ADR-0037](../../../docs/adr/0037-api-keys.md)): high-entropy, `SHA-256`-hashed
//! (not Argon2 — there is no low-entropy password to slow down), and shown exactly once.

use core::future::Future;

use sha2::{Digest, Sha256};

use pos_core::activation::{ActivationCode, CodeStatus};
use pos_proto::ids::{DeviceId, StoreId, TenantId};

// Minting a device credential moved to pos-cloud-api with the bearer check that reads it
// (ADR-0170 decision 4); re-exported so `crate::activation::…` still names it.
pub use crate::auth::device_credential::{DeviceCredential, mint_device_credential};

/// `SHA-256` of an activation code's canonical text — what the store keys on, never the code itself.
///
/// Hashing the canonical form (not the operator's raw keystrokes) means a code issued and a code
/// presented hash alike however each was spaced or cased, since [`ActivationCode`] normalises both.
#[must_use]
pub fn hash_code(code: &ActivationCode) -> [u8; 32] {
    Sha256::digest(code.as_str().as_bytes()).into()
}

/// What a looked-up activation code carries: the slot it belongs to and its lifecycle state.
#[derive(Debug, Clone)]
pub struct IssuedCode {
    /// The tenant the code — and the credential it mints — belongs to.
    pub tenant_id: TenantId,
    /// The store the code belongs to.
    pub store_id: StoreId,
    /// The device slot the code activates.
    pub device_id: DeviceId,
    /// The code's current lifecycle state, which [`pos_core::activation::redeem`] weighs.
    pub status: CodeStatus,
}

/// The store behind the activation exchange (a table in `store-postgres`; a fake in tests).
///
/// A miss on [`Self::lookup`] is `Ok(None)`, not an error — an unknown code is refused exactly as a
/// spent one is, so a prober cannot tell them apart. An [`ActivationStoreError`] means the backing
/// store itself failed — the database is unreachable — which is the caller's cue to answer retryably
/// rather than deny.
pub trait ActivationCodeStore {
    /// Persists a freshly issued code — its hash and the slot it is bound to.
    ///
    /// # Errors
    ///
    /// [`ActivationStoreError`] if the store could not be written.
    fn issue(
        &self,
        code_hash: [u8; 32],
        tenant_id: TenantId,
        store_id: StoreId,
        device_id: DeviceId,
    ) -> impl Future<Output = Result<(), ActivationStoreError>> + Send;

    /// Looks up a code by its hash, or `None` if there is no such code.
    ///
    /// # Errors
    ///
    /// [`ActivationStoreError`] only if the store itself could not be read — never for a missing code.
    fn lookup(
        &self,
        code_hash: [u8; 32],
    ) -> impl Future<Output = Result<Option<IssuedCode>, ActivationStoreError>> + Send;

    /// Redeems the code and mints `credential` in one transaction, returning whether the code was
    /// still issued (and so consumed). `Ok(false)` means it was already spent, revoked, or lost a
    /// race — the caller then refuses, exactly as for an unknown code.
    ///
    /// # Errors
    ///
    /// [`ActivationStoreError`] if the store could not be written.
    fn consume_and_provision(
        &self,
        code_hash: [u8; 32],
        credential: &DeviceCredential,
    ) -> impl Future<Output = Result<bool, ActivationStoreError>> + Send;

    /// Revokes every still-issued code for a device slot, returning how many were cancelled — the
    /// leaked-setup-sheet path. Idempotent: a slot with no issued codes returns `0`.
    ///
    /// # Errors
    ///
    /// [`ActivationStoreError`] if the store could not be written.
    fn revoke_slot(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
        device_id: DeviceId,
    ) -> impl Future<Output = Result<u64, ActivationStoreError>> + Send;
}

/// A failure of the activation store itself — the database is unreachable — as distinct from a code
/// simply not being present, which is `Ok(None)`.
#[derive(Debug, thiserror::Error)]
#[error("the activation store failed: {0}")]
pub struct ActivationStoreError(String);

impl ActivationStoreError {
    /// A store failure carrying a human-readable reason (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[cfg(test)]
mod tests {
    use super::hash_code;

    use pos_core::activation::ActivationCode;

    #[test]
    fn hashing_the_code_is_stable_across_formatting() {
        let code = ActivationCode::from_entropy([7; pos_core::activation::PAYLOAD_LEN]);
        // The same code presented lower-case and spaced hashes identically, because parse normalises.
        let typed = ActivationCode::parse(&code.as_str().to_lowercase().replace('-', " "))
            .expect("a valid code");
        assert_eq!(hash_code(&code), hash_code(&typed));
    }
}
