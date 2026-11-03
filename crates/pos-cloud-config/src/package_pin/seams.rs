// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the settings routes read beside the settings themselves to write and compile the `package`
//! node: the tenant's packages, and the release each store last reported.

use core::future::Future;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use pos_proto::ids::{StoreId, TenantId};
use pos_proto::settings::SettingScope;

use crate::packages::PackageStore;
use crate::settings::{SettingValue, SettingsStore, SettingsStoreError};

/// The release each store last reported running
/// ([ADR-0078](../../../../docs/adr/0078-sync-and-ota-closure.md)), which decides whether a package
/// version is compiled into its document (ADR-0189 decision 5).
pub trait StoreReleases {
    /// The release `store_id` last reported, as it reported it, or `None` if it never has.
    ///
    /// # Errors
    ///
    /// A message for the server's log when the release could not be read.
    fn installed_release(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Option<String>, String>> + Send;
}

/// The most stores [`InMemoryReleases`] holds a release for.
pub const IN_MEMORY_RELEASES: usize = 4096;

/// Releases in memory, for tests and for code that runs without a fleet. Holds at most
/// [`IN_MEMORY_RELEASES`] stores; a report beyond that is dropped.
#[derive(Clone, Debug, Default)]
pub struct InMemoryReleases {
    releases: Arc<Mutex<BTreeMap<(TenantId, StoreId), String>>>,
}

impl InMemoryReleases {
    /// No store has reported.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that `store_id` runs `release`.
    pub fn report(&self, tenant_id: TenantId, store_id: StoreId, release: &str) {
        let mut releases = self.releases.lock().unwrap_or_else(PoisonError::into_inner);
        if releases.len() < IN_MEMORY_RELEASES || releases.contains_key(&(tenant_id, store_id)) {
            releases.insert((tenant_id, store_id), release.to_owned());
        }
    }
}

impl StoreReleases for InMemoryReleases {
    async fn installed_release(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
    ) -> Result<Option<String>, String> {
        Ok(self
            .releases
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(tenant_id, store_id))
            .cloned())
    }
}

/// A settings store that can also read what the `package` values name and what each store runs.
pub trait SettingsPackages {
    /// The package store.
    type Packages: PackageStore + Sync;
    /// Where each store's release is read.
    type Releases: StoreReleases + Sync;

    /// The tenant's packages.
    fn packages(&self) -> &Self::Packages;

    /// The release each store last reported.
    fn releases(&self) -> &Self::Releases;
}

/// A settings store with the tenant's packages and the stores' releases beside it: the settings
/// routes' store, so a write of a `package` value can check the versions it names, and a publish
/// can compile the pinned version into a store's document.
#[derive(Clone, Debug)]
pub struct WithPackages<St, P, R> {
    settings: St,
    packages: P,
    releases: R,
}

impl<St, P, R> WithPackages<St, P, R> {
    /// The settings store `settings`, with `packages` and `releases` beside it.
    #[must_use]
    pub const fn new(settings: St, packages: P, releases: R) -> Self {
        Self {
            settings,
            packages,
            releases,
        }
    }
}

impl<St, P: PackageStore + Sync, R: StoreReleases + Sync> SettingsPackages
    for WithPackages<St, P, R>
{
    type Packages = P;
    type Releases = R;

    fn packages(&self) -> &P {
        &self.packages
    }

    fn releases(&self) -> &R {
        &self.releases
    }
}

impl<St: SettingsStore + Sync, P: Sync, R: Sync> SettingsStore for WithPackages<St, P, R> {
    fn list(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<SettingValue>, SettingsStoreError>> + Send {
        self.settings.list(tenant_id)
    }

    fn put(
        &self,
        tenant_id: TenantId,
        value: &SettingValue,
    ) -> impl Future<Output = Result<(), SettingsStoreError>> + Send {
        self.settings.put(tenant_id, value)
    }

    fn delete(
        &self,
        tenant_id: TenantId,
        setting_key: &str,
        scope: SettingScope,
        scope_id: &str,
    ) -> impl Future<Output = Result<bool, SettingsStoreError>> + Send {
        self.settings
            .delete(tenant_id, setting_key, scope, scope_id)
    }
}
