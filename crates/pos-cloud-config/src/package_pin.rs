// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the `package` settings obey beyond the register
//! ([ADR-0189](../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 2).
//!
//! - **A lock holds below it.** While a scope that reaches a store sets `package.mode` to
//!   `PACKAGE_MODE_LOCKED`, no narrower scope's `package` value reaches that store, and a write of
//!   one is refused. A value written before the lock is kept, and held back while it lasts. This
//!   is lock by path (roadmap-v3 B7.2) for the `package` node only; the general rule for every node
//!   is phase 4's.
//! - **The choice is the store's.** `package.chosen_version_id` is written only through the choose
//!   route, at the store, and only with a version the store is allowed.

use core::future::Future;

use pos_proto::ids::TenantId;
use pos_proto::package::PackageMode;
use pos_proto::settings::SettingScope;
use pos_proto::wire_enum::WireEnum as _;

use crate::packages::PackageStore;
use crate::settings::{
    ResolvedSetting, SettingValue, SettingsStore, SettingsStoreError, StorePlacement, rank_at,
};

/// The node the package settings are on, as a key prefix.
const PREFIX: &str = "package.";

/// `package.mode`.
pub const MODE_KEY: &str = "package.mode";

/// `package.chosen_version_id`.
pub const CHOSEN_KEY: &str = "package.chosen_version_id";

/// `package.allowed_version_ids`.
pub const ALLOWED_KEY: &str = "package.allowed_version_ids";

/// Whether `setting_key` is one of the `package` node's settings.
#[must_use]
pub fn is_package_key(setting_key: &str) -> bool {
    setting_key.starts_with(PREFIX)
}

/// The broadest rank, as [`rank_at`] counts it, at which a scope reaching the store at `placement`
/// locks its package, or `None` when nothing does.
#[must_use]
pub fn broadest_lock(values: &[SettingValue], placement: &StorePlacement) -> Option<u8> {
    values
        .iter()
        .filter(|value| {
            value.setting_key == MODE_KEY
                && value.value.as_str() == Some(PackageMode::Locked.as_wire())
        })
        .filter_map(|value| rank_at(placement, value.scope.known(), &value.scope_id))
        .max()
}

/// Whether a `package` value written at `scope` would sit under a lock at a broader scope that
/// reaches the store at `placement`. A write at the lock's own scope is not under it: that is how
/// the lock is changed or lifted.
#[must_use]
pub fn locked_above(
    values: &[SettingValue],
    placement: &StorePlacement,
    scope: SettingScope,
) -> bool {
    let written_at = match scope {
        SettingScope::Store => 0,
        SettingScope::StoreGroup => 1,
        SettingScope::Brand => 2,
        SettingScope::Tenant => 3,
        SettingScope::Unspecified => return false,
    };
    broadest_lock(values, placement).is_some_and(|locked_at| written_at < locked_at)
}

/// The store's resolved `package` value for `field`, if any.
fn resolved<'a>(resolved: &'a [ResolvedSetting], field: &str) -> Option<&'a serde_json::Value> {
    resolved
        .iter()
        .find(|setting| setting.node == "package" && setting.field == field)
        .map(|setting| &setting.value)
}

/// Whether the store's resolved settings let it choose `package_version_id`: its mode is
/// `PACKAGE_MODE_CHOOSE` and the version is one of its allowed versions.
#[must_use]
pub fn may_choose(settings: &[ResolvedSetting], package_version_id: &str) -> Option<ChoiceRefusal> {
    if resolved(settings, "mode").and_then(serde_json::Value::as_str)
        != Some(PackageMode::Choose.as_wire())
    {
        return Some(ChoiceRefusal::NotChoosing);
    }
    let allowed = resolved(settings, "allowed_version_ids")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(package_version_id)));
    (!allowed).then_some(ChoiceRefusal::NotAllowed)
}

/// Why a store cannot choose a version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChoiceRefusal {
    /// The store's package is not in `PACKAGE_MODE_CHOOSE`.
    NotChoosing,
    /// The version is not one the store is allowed.
    NotAllowed,
}

/// A settings store that can also read the package versions its `package` values name, which a
/// write of one checks.
pub trait SettingsPackages {
    /// The package store.
    type Packages: PackageStore + Sync;

    /// The tenant's packages.
    fn packages(&self) -> &Self::Packages;
}

/// A settings store with the tenant's package store beside it: the settings routes' store, so a
/// write of a `package` value can check the versions it names.
#[derive(Clone, Debug)]
pub struct WithPackages<St, P> {
    settings: St,
    packages: P,
}

impl<St, P> WithPackages<St, P> {
    /// The settings store `settings`, with `packages` beside it.
    #[must_use]
    pub const fn new(settings: St, packages: P) -> Self {
        Self { settings, packages }
    }
}

impl<St, P: PackageStore + Sync> SettingsPackages for WithPackages<St, P> {
    type Packages = P;

    fn packages(&self) -> &P {
        &self.packages
    }
}

impl<St: SettingsStore + Sync, P: Sync> SettingsStore for WithPackages<St, P> {
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

#[cfg(test)]
mod tests;
