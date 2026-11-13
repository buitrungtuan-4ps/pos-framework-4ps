// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Sealing a tenant's secrets under its own key (ADR-0217 decisions 1 and 2).
//!
//! A value sealed under tenant-key version `n` is stored as `t<n>.` followed by the hex of the nonce
//! and the ciphertext, bound to the tenant, the connection or endpoint and the field. A value with
//! no prefix is the format before tenant keys, version 0, sealed directly under
//! `integration_secret`, and still opens while that key is configured.
//!
//! [`Keys::for_tenant`] reads the tenant's versions and unwraps them through the environment key,
//! generating version 1 on the tenant's first need, and hands back a [`TenantSealer`] that seals
//! under the newest and opens under any it holds. The sealer holds the keys in memory for one
//! request and zeroizes them when dropped; nothing here logs a key or a value.

use core::fmt;
use core::future::Future;
use core::pin::Pin;
use std::collections::BTreeMap;
use std::sync::Arc;

use zeroize::Zeroizing;

use pos_ports::key_keeper::KeyKeeper;
use pos_ports::key_vault::Secret;
use pos_proto::ClockSource;
use pos_proto::ids::TenantId;

use super::tenant::{TenantKey, TenantKeyStore};
use crate::connections::{
    ConnectionSecret, KEY_LEN, SealError, SealedFor, open, open_under, seal_under,
};

/// What every seal under a tenant key is bound to besides its tenant, connection and field.
const TENANT_SEAL_CONTEXT: &[u8] = b"pos-cloud/tenant-secret/v1";

/// The most versions of one tenant's key a sealer opens under: a rotation retires a version once
/// nothing is sealed under it, so a tenant has a few at most.
pub const KEY_VERSIONS_MAX: usize = 16;

/// The tenant-key version a stored value names: `0` for a value from before tenant keys.
#[must_use]
pub fn sealed_version(stored: &str) -> Option<u32> {
    match stored.strip_prefix('t') {
        None => Some(0),
        Some(rest) => rest
            .split_once('.')
            .and_then(|(version, _)| version.parse().ok()),
    }
}

/// One tenant's keys, unwrapped for one request.
pub struct TenantSealer {
    legacy: Option<ConnectionSecret>,
    keys: BTreeMap<u32, Zeroizing<[u8; KEY_LEN]>>,
}

impl fmt::Debug for TenantSealer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TenantSealer")
            .field("versions", &self.keys.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl TenantSealer {
    /// Seals `value` under the tenant's newest key, as `t<version>.<hex>`.
    ///
    /// # Errors
    ///
    /// [`SealError::Key`] if the sealer holds no key, or [`SealError::Entropy`] if no nonce could
    /// be drawn.
    pub fn seal(&self, sealed_for: SealedFor<'_>, value: &str) -> Result<String, SealError> {
        let (version, key) = self.keys.iter().next_back().ok_or(SealError::Key)?;
        let sealed = seal_under(key, &sealed_for.bound_under(TENANT_SEAL_CONTEXT), value)?;
        Ok(format!("t{version}.{sealed}"))
    }

    /// Opens a value sealed under any version this sealer holds, or under `integration_secret`
    /// for a value from before tenant keys.
    ///
    /// # Errors
    ///
    /// [`SealError::Malformed`] if `stored` is not a sealed value, or [`SealError::Key`] if the
    /// version is not held or the value does not open under it for this tenant, connection and
    /// field.
    pub fn open(
        &self,
        sealed_for: SealedFor<'_>,
        stored: &str,
    ) -> Result<Zeroizing<String>, SealError> {
        match sealed_version(stored).ok_or(SealError::Malformed)? {
            0 => open(
                self.legacy.as_ref().ok_or(SealError::Key)?,
                sealed_for,
                stored,
            ),
            version => {
                let key = self.keys.get(&version).ok_or(SealError::Key)?;
                let body = stored
                    .split_once('.')
                    .map(|(_, body)| body)
                    .ok_or(SealError::Malformed)?;
                open_under(key, &sealed_for.bound_under(TENANT_SEAL_CONTEXT), body)
            }
        }
    }

    /// The newest version this sealer seals under.
    #[must_use]
    pub fn newest_version(&self) -> u32 {
        self.keys.keys().next_back().copied().unwrap_or_default()
    }
}

/// A boxed future, so [`KeySource`] stays object-safe.
type KeyFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SealError>> + Send + 'a>>;

/// Where a tenant's sealer comes from, and what changes its keys, object-safe so a router holds it
/// without the keeper's and the store's types.
trait KeySource: Send + Sync {
    fn for_tenant(&self, tenant_id: TenantId) -> KeyFuture<'_, TenantSealer>;
    fn rotate(&self, tenant_id: TenantId) -> KeyFuture<'_, u32>;
    fn rewrap(&self, tenant_id: TenantId) -> KeyFuture<'_, usize>;
    fn delete(&self, tenant_id: TenantId, below: Option<u32>) -> KeyFuture<'_, u64>;
}

/// The keys every secret is sealed under: an environment keeper, the tenant-key store, and
/// `integration_secret` for values from before tenant keys. Cheap to clone.
#[derive(Clone)]
pub struct Keys(Arc<dyn KeySource>);

impl fmt::Debug for Keys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Keys(…)")
    }
}

impl Keys {
    /// Keys over `keeper` and `store`, opening a value from before tenant keys under `legacy`.
    #[must_use]
    pub fn new<K, S, C>(keeper: K, store: S, legacy: Option<ConnectionSecret>, clock: C) -> Self
    where
        K: KeyKeeper + 'static,
        S: TenantKeyStore + 'static,
        C: ClockSource + Send + Sync + 'static,
    {
        Self(Arc::new(Envelope {
            keeper,
            store,
            legacy,
            clock,
        }))
    }

    /// The tenant's sealer: its keys unwrapped, version 1 generated on its first need.
    ///
    /// # Errors
    ///
    /// [`SealError::Keys`] if the keys cannot be read, made or unwrapped.
    pub async fn for_tenant(&self, tenant_id: TenantId) -> Result<TenantSealer, SealError> {
        self.0.for_tenant(tenant_id).await
    }

    /// Creates the tenant's next key version, which new seals use from now on; older versions keep
    /// opening until the re-seal pass retires them (ADR-0217 decision 3). Answers the new version.
    ///
    /// # Errors
    ///
    /// [`SealError::Keys`] if the keys cannot be read, made or stored.
    pub async fn rotate_tenant(&self, tenant_id: TenantId) -> Result<u32, SealError> {
        self.0.rotate(tenant_id).await
    }

    /// Re-wraps each of the tenant's key versions that an older environment-key version wrapped
    /// under the keeper's newest: the tenant's half of rotating the environment key. Answers how
    /// many were re-wrapped.
    ///
    /// # Errors
    ///
    /// [`SealError::Keys`] if a key cannot be read, unwrapped, wrapped or stored.
    pub async fn rewrap_tenant(&self, tenant_id: TenantId) -> Result<usize, SealError> {
        self.0.rewrap(tenant_id).await
    }

    /// Retires the tenant's key versions below `version`. Only the re-seal pass calls it, once
    /// nothing is sealed under them.
    pub(crate) async fn retire_below(
        &self,
        tenant_id: TenantId,
        version: u32,
    ) -> Result<u64, SealError> {
        self.0.delete(tenant_id, Some(version)).await
    }

    /// Deletes every version of the tenant's key, which makes every secret sealed under them
    /// unreadable at once and cannot be undone (ADR-0217 decision 4). The caller has the owner's
    /// confirmation and audits it: `pos-cloud keys erase` is the one caller. A value from before
    /// tenant keys is not under them, and the re-seal pass is what moves those first.
    ///
    /// # Errors
    ///
    /// [`SealError::Keys`] if the store cannot be reached.
    pub async fn erase_tenant(&self, tenant_id: TenantId) -> Result<u64, SealError> {
        self.0.delete(tenant_id, None).await
    }
}

/// Logs a key-store failure, by its message only, and answers [`SealError::Keys`].
fn store_failed(error: &super::tenant::TenantKeyStoreError) -> SealError {
    tracing::error!(%error, "the tenant key store failed");
    SealError::Keys
}

struct Envelope<K, S, C> {
    keeper: K,
    store: S,
    legacy: Option<ConnectionSecret>,
    clock: C,
}

impl<K, S, C> Envelope<K, S, C>
where
    K: KeyKeeper,
    S: TenantKeyStore,
    C: ClockSource + Send + Sync,
{
    /// The tenant's versions, generating version 1 if it has none.
    async fn versions(&self, tenant_id: TenantId) -> Result<Vec<TenantKey>, SealError> {
        let held = self.store.versions(tenant_id).await.map_err(|error| {
            tracing::error!(%error, "a tenant's keys could not be read");
            SealError::Keys
        })?;
        if !held.is_empty() {
            return Ok(held);
        }
        let mut key = Zeroizing::new([0_u8; KEY_LEN]);
        getrandom::fill(key.as_mut_slice()).map_err(|_error| SealError::Entropy)?;
        let wrapped = self
            .keeper
            .wrap(&Secret::new(key.to_vec()))
            .await
            .map_err(|error| {
                tracing::error!(%error, "a tenant key could not be wrapped");
                SealError::Keys
            })?;
        let first = TenantKey {
            key_version: 1,
            wrapped,
            created_at_ms: self.clock.now().as_milliseconds_since_epoch(),
        };
        // Another request may have made it first; then its version 1 is the tenant's.
        self.store
            .insert(tenant_id, &first)
            .await
            .map_err(|error| {
                tracing::error!(%error, "a tenant key could not be stored");
                SealError::Keys
            })?;
        self.store.versions(tenant_id).await.map_err(|error| {
            tracing::error!(%error, "a tenant's keys could not be read");
            SealError::Keys
        })
    }

    async fn sealer(&self, tenant_id: TenantId) -> Result<TenantSealer, SealError> {
        let versions = self.versions(tenant_id).await?;
        let skip = versions.len().saturating_sub(KEY_VERSIONS_MAX);
        let mut keys = BTreeMap::new();
        for held in versions.iter().skip(skip) {
            let opened = self.keeper.unwrap(&held.wrapped).await.map_err(|error| {
                tracing::error!(
                    %error,
                    key_version = held.key_version,
                    keeper_version = held.wrapped.keeper_version,
                    "a tenant key could not be unwrapped"
                );
                SealError::Keys
            })?;
            let key: [u8; KEY_LEN] = opened
                .expose()
                .try_into()
                .map_err(|_error| SealError::Keys)?;
            keys.insert(held.key_version, Zeroizing::new(key));
        }
        Ok(TenantSealer {
            legacy: self.legacy.clone(),
            keys,
        })
    }
}

impl<K, S, C> Envelope<K, S, C>
where
    K: KeyKeeper,
    S: TenantKeyStore,
    C: ClockSource + Send + Sync,
{
    async fn next_version(&self, tenant_id: TenantId) -> Result<u32, SealError> {
        let held = self.versions(tenant_id).await?;
        let next = held
            .last()
            .map_or(1, |newest| newest.key_version.saturating_add(1));
        let mut key = Zeroizing::new([0_u8; KEY_LEN]);
        getrandom::fill(key.as_mut_slice()).map_err(|_error| SealError::Entropy)?;
        let wrapped = self
            .keeper
            .wrap(&Secret::new(key.to_vec()))
            .await
            .map_err(|error| {
                tracing::error!(%error, "a tenant key could not be wrapped");
                SealError::Keys
            })?;
        let created_at_ms = self.clock.now().as_milliseconds_since_epoch();
        let stored = self
            .store
            .insert(
                tenant_id,
                &TenantKey {
                    key_version: next,
                    wrapped,
                    created_at_ms,
                },
            )
            .await
            .map_err(|error| store_failed(&error))?;
        if stored {
            tracing::info!(key_version = next, "a tenant key was rotated");
            Ok(next)
        } else {
            tracing::error!(
                key_version = next,
                "another writer made this tenant key version first"
            );
            Err(SealError::Keys)
        }
    }

    async fn rewrapped(&self, tenant_id: TenantId) -> Result<usize, SealError> {
        let mut moved = 0;
        for held in self
            .store
            .versions(tenant_id)
            .await
            .map_err(|error| store_failed(&error))?
        {
            let key = self.keeper.unwrap(&held.wrapped).await.map_err(|error| {
                tracing::error!(%error, key_version = held.key_version, "a tenant key could not be unwrapped");
                SealError::Keys
            })?;
            let wrapped = self.keeper.wrap(&key).await.map_err(|error| {
                tracing::error!(%error, "a tenant key could not be wrapped");
                SealError::Keys
            })?;
            if wrapped.keeper_version != held.wrapped.keeper_version {
                let again = TenantKey { wrapped, ..held };
                if self
                    .store
                    .rewrap(tenant_id, &again)
                    .await
                    .map_err(|error| store_failed(&error))?
                {
                    moved += 1;
                }
            }
        }
        Ok(moved)
    }
}

impl<K, S, C> KeySource for Envelope<K, S, C>
where
    K: KeyKeeper,
    S: TenantKeyStore,
    C: ClockSource + Send + Sync,
{
    fn for_tenant(&self, tenant_id: TenantId) -> KeyFuture<'_, TenantSealer> {
        Box::pin(self.sealer(tenant_id))
    }

    fn rotate(&self, tenant_id: TenantId) -> KeyFuture<'_, u32> {
        Box::pin(self.next_version(tenant_id))
    }

    fn rewrap(&self, tenant_id: TenantId) -> KeyFuture<'_, usize> {
        Box::pin(self.rewrapped(tenant_id))
    }

    fn delete(&self, tenant_id: TenantId, below: Option<u32>) -> KeyFuture<'_, u64> {
        Box::pin(async move {
            self.store
                .delete(tenant_id, below)
                .await
                .map_err(|error| store_failed(&error))
        })
    }
}

#[cfg(test)]
mod tests;
