// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Wiring pos-cloud-config's persistence seams to their `store-postgres` tables: the
//! `ConfigTreeStore` ([ADR-0033](../../../docs/adr/0033-config-tree.md)), the `RegistryStore`
//! ([ADR-0065](../../../docs/adr/0065-cloud-org-registry.md)), the `SettingsStore` and the
//! `DataMigrations` record.
//!
//! Moved out of `pos-cloud`'s `persistence.rs` with the traits (ADR-0170 decision 4): a trait can be
//! implemented for another crate's type only in the trait's own crate. The SQL stays in
//! `store-postgres`. The registry's id parsers and the two conditional-write translations are `pub`,
//! because `pos-cloud`'s own impls use them too.

use store_postgres::{
    BrandRow, DeviceRow, PostgresConfigTrees, PostgresDataMigrations, PostgresRegistry,
    PostgresSettings, RowUpdate, SettingSlot, StoreRow, TenantRow,
};

use pos_proto::ids::{ConfigVersionId, DeviceId, StoreId, TenantId};
use pos_proto::settings::SettingScope;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::WireEnum as _;

use crate::config_tree::{ConfigStoreError, ConfigTreeState, ConfigTreeStore, PrintAgentStanding};
use crate::registry::{
    BrandId, BrandRecord, DeviceRecord, EntityStatus, RegistryStore, RegistryStoreError,
    StoreRecord, TenantRecord,
};
use crate::settings::{SettingValue, SettingsStore, SettingsStoreError};
use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned};

impl ConfigTreeStore for PostgresConfigTrees {
    async fn load(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> Result<Option<Versioned<ConfigTreeState>>, ConfigStoreError> {
        match self
            .load_state(tenant, store)
            .await
            .map_err(|error| ConfigStoreError::new(error.to_string()))?
        {
            Some((json, version)) => serde_json::from_str(&json)
                .map(|state| Some(Versioned::new(state, Version::new(version))))
                .map_err(|error| {
                    ConfigStoreError::new(format!(
                        "decoding the stored config tree failed: {error}"
                    ))
                }),
            None => Ok(None),
        }
    }

    async fn save(
        &self,
        tenant: TenantId,
        store: StoreId,
        state: &ConfigTreeState,
        expected: Option<&Version>,
    ) -> Result<UpdateOutcome, ConfigStoreError> {
        let json = serde_json::to_string(state).map_err(|error| {
            ConfigStoreError::new(format!("encoding the config tree failed: {error}"))
        })?;
        self.save_state(tenant, store, &json, expected.map(Version::as_str))
            .await
            .map(update_outcome)
            .map_err(|error| ConfigStoreError::new(error.to_string()))
    }

    async fn record_store_seen(
        &self,
        tenant: TenantId,
        store: StoreId,
        held_version: Option<ConfigVersionId>,
        seen_at: Timestamp,
    ) -> Result<(), ConfigStoreError> {
        let held = held_version.map(|version| version.to_string());
        self.record_seen(
            tenant,
            store,
            held.as_deref(),
            seen_at.as_milliseconds_since_epoch(),
        )
        .await
        .map_err(|error| ConfigStoreError::new(error.to_string()))
    }

    async fn record_store_heartbeat(
        &self,
        tenant: TenantId,
        store: StoreId,
        seen_at: Timestamp,
        outbox_depth: Option<u64>,
        lease_generation: Option<u64>,
        print_agents: Option<Vec<PrintAgentStanding>>,
    ) -> Result<(), ConfigStoreError> {
        // A depth past `i64::MAX` is not reachable from a store's log, but saturating beats a panic
        // and beats dropping the heartbeat: the column is `bigint`, so this is the widest it holds.
        let depth = outbox_depth.map(|depth| i64::try_from(depth).unwrap_or(i64::MAX));
        // Same rule for the generation, and just as unreachable: a store issues a handful of leases
        // in its life, and `LeaseGeneration::next` saturates rather than wrapping (ADR-0049).
        let generation = lease_generation.map(|value| i64::try_from(value).unwrap_or(i64::MAX));
        // Encoded here rather than in the adapter, so `store-postgres` keeps knowing nothing about
        // what the JSON it moves means. An encode that fails cannot happen for two strings and a
        // number, but it is reported rather than silently dropping the standings: a beat that
        // silently omitted them would read as "did not look" and leave a stale list on the console.
        let print_agents = print_agents
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| ConfigStoreError::new(error.to_string()))?;
        self.record_heartbeat(
            tenant,
            store,
            seen_at.as_milliseconds_since_epoch(),
            depth,
            generation,
            print_agents,
        )
        .await
        .map_err(|error| ConfigStoreError::new(error.to_string()))
    }
}

// --- The org registry (ADR-0065) ---------------------------------------------------------------

/// Parses a tenant id the registry stored.
///
/// # Errors
///
/// [`RegistryStoreError`] if a stored tenant id is not a ULID.
pub fn parse_registry_tenant(text: &str) -> Result<TenantId, RegistryStoreError> {
    text.parse::<Ulid>().map(TenantId::new).map_err(|_ignored| {
        RegistryStoreError::new(format!("a registry tenant id is not a ULID: {text}"))
    })
}

fn parse_registry_brand(text: &str) -> Result<BrandId, RegistryStoreError> {
    text.parse::<Ulid>().map(BrandId::new).map_err(|_ignored| {
        RegistryStoreError::new(format!("a registry brand id is not a ULID: {text}"))
    })
}

/// Parses a store id the registry stored.
///
/// # Errors
///
/// [`RegistryStoreError`] if a stored store id is not a ULID.
pub fn parse_registry_store(text: &str) -> Result<StoreId, RegistryStoreError> {
    text.parse::<Ulid>().map(StoreId::new).map_err(|_ignored| {
        RegistryStoreError::new(format!("a registry store id is not a ULID: {text}"))
    })
}

fn parse_registry_device(text: &str) -> Result<DeviceId, RegistryStoreError> {
    text.parse::<Ulid>().map(DeviceId::new).map_err(|_ignored| {
        RegistryStoreError::new(format!("a registry device id is not a ULID: {text}"))
    })
}

fn tenant_record(row: TenantRow) -> Result<Versioned<TenantRecord>, RegistryStoreError> {
    let version = Version::new(row.version);
    Ok(Versioned::new(
        TenantRecord {
            tenant_id: parse_registry_tenant(&row.tenant_id)?,
            name: row.name,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn brand_record(row: BrandRow) -> Result<Versioned<BrandRecord>, RegistryStoreError> {
    let version = Version::new(row.version);
    Ok(Versioned::new(
        BrandRecord {
            brand_id: parse_registry_brand(&row.brand_id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)?,
            name: row.name,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn store_record(row: StoreRow) -> Result<Versioned<StoreRecord>, RegistryStoreError> {
    let brand_id = match row.brand_id {
        Some(text) => Some(parse_registry_brand(&text)?),
        None => None,
    };
    let version = Version::new(row.version);
    Ok(Versioned::new(
        StoreRecord {
            store_id: parse_registry_store(&row.store_id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)?,
            brand_id,
            name: row.name,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn device_record(row: DeviceRow) -> Result<Versioned<DeviceRecord>, RegistryStoreError> {
    let version = Version::new(row.version);
    Ok(Versioned::new(
        DeviceRecord {
            device_id: parse_registry_device(&row.device_id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)?,
            store_id: parse_registry_store(&row.store_id)?,
            name: row.name,
            kind: row.kind,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

/// Carries the adapter's conditional-write result across the seam
/// ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)).
///
/// The two types are deliberately separate rather than one shared enum: `RowUpdate` names a
/// database row and `UpdateOutcome` names a seam's answer, and this crate is the only place that
/// knows both. Collapsing them would put a `store-postgres` type in the seam every fork has to
/// implement. One function serves every seam — the translation does not vary by entity, and a copy
/// per store would be a copy that can drift.
#[must_use]
pub fn update_outcome(update: RowUpdate) -> UpdateOutcome {
    match update {
        RowUpdate::Updated(version) => UpdateOutcome::Updated(Version::new(version)),
        RowUpdate::VersionMismatch => UpdateOutcome::VersionMismatch,
        RowUpdate::NotFound => UpdateOutcome::NotFound,
    }
}

/// Translates an insert's `Option<version>` into the seam's [`CreateOutcome`].
///
/// The `None` comes from `ON CONFLICT DO NOTHING ... RETURNING` writing nothing, which is
/// `store-postgres`'s way of saying the key was taken. Kept beside [`update_outcome`] and for the
/// same reason: this crate is the only place that knows both vocabularies, and one function per
/// seam would be a copy that can drift.
#[must_use]
pub fn create_outcome(inserted: Option<String>) -> CreateOutcome {
    inserted.map_or(CreateOutcome::AlreadyExists, |version| {
        CreateOutcome::Created(Version::new(version))
    })
}

impl RegistryStore for PostgresRegistry {
    async fn create_tenant(&self, tenant: &TenantRecord) -> Result<Version, RegistryStoreError> {
        self.insert_tenant(&tenant.tenant_id.to_string(), &tenant.name)
            .await
            .map(Version::new)
            .map_err(|error| RegistryStoreError::new(error.to_string()))
    }

    async fn list_tenants(&self) -> Result<Vec<Versioned<TenantRecord>>, RegistryStoreError> {
        let rows = self
            .fetch_tenants()
            .await
            .map_err(|error| RegistryStoreError::new(error.to_string()))?;
        rows.into_iter().map(tenant_record).collect()
    }

    async fn update_tenant(
        &self,
        tenant: &TenantRecord,
        expected: &Version,
    ) -> Result<UpdateOutcome, RegistryStoreError> {
        self.set_tenant(
            &tenant.tenant_id.to_string(),
            &tenant.name,
            tenant.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| RegistryStoreError::new(error.to_string()))
    }

    async fn create_brand(&self, brand: &BrandRecord) -> Result<Version, RegistryStoreError> {
        self.insert_brand(
            &brand.brand_id.to_string(),
            &brand.tenant_id.to_string(),
            &brand.name,
        )
        .await
        .map(Version::new)
        .map_err(|error| RegistryStoreError::new(error.to_string()))
    }

    async fn list_brands(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<BrandRecord>>, RegistryStoreError> {
        let rows = self
            .fetch_brands(&tenant_id.to_string())
            .await
            .map_err(|error| RegistryStoreError::new(error.to_string()))?;
        rows.into_iter().map(brand_record).collect()
    }

    async fn update_brand(
        &self,
        brand: &BrandRecord,
        expected: &Version,
    ) -> Result<UpdateOutcome, RegistryStoreError> {
        self.set_brand(
            &brand.tenant_id.to_string(),
            &brand.brand_id.to_string(),
            &brand.name,
            brand.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| RegistryStoreError::new(error.to_string()))
    }

    async fn create_store(&self, store: &StoreRecord) -> Result<Version, RegistryStoreError> {
        let brand = store.brand_id.map(|brand_id| brand_id.to_string());
        self.insert_store(
            &store.store_id.to_string(),
            &store.tenant_id.to_string(),
            brand.as_deref(),
            &store.name,
        )
        .await
        .map(Version::new)
        .map_err(|error| RegistryStoreError::new(error.to_string()))
    }

    async fn list_stores(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<StoreRecord>>, RegistryStoreError> {
        let rows = self
            .fetch_stores(&tenant_id.to_string())
            .await
            .map_err(|error| RegistryStoreError::new(error.to_string()))?;
        rows.into_iter().map(store_record).collect()
    }

    async fn update_store(
        &self,
        store: &StoreRecord,
        expected: &Version,
    ) -> Result<UpdateOutcome, RegistryStoreError> {
        let brand = store.brand_id.map(|brand_id| brand_id.to_string());
        self.set_store(
            &store.tenant_id.to_string(),
            &store.store_id.to_string(),
            brand.as_deref(),
            &store.name,
            store.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| RegistryStoreError::new(error.to_string()))
    }

    async fn create_device(&self, device: &DeviceRecord) -> Result<Version, RegistryStoreError> {
        self.insert_device(
            &device.device_id.to_string(),
            &device.tenant_id.to_string(),
            &device.store_id.to_string(),
            &device.name,
            &device.kind,
        )
        .await
        .map(Version::new)
        .map_err(|error| RegistryStoreError::new(error.to_string()))
    }

    async fn list_devices(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Versioned<DeviceRecord>>, RegistryStoreError> {
        let rows = self
            .fetch_devices(&tenant_id.to_string(), &store_id.to_string())
            .await
            .map_err(|error| RegistryStoreError::new(error.to_string()))?;
        rows.into_iter().map(device_record).collect()
    }

    async fn update_device(
        &self,
        device: &DeviceRecord,
        expected: &Version,
    ) -> Result<UpdateOutcome, RegistryStoreError> {
        self.set_device(
            &device.tenant_id.to_string(),
            &device.device_id.to_string(),
            &device.name,
            &device.kind,
            device.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| RegistryStoreError::new(error.to_string()))
    }
}

impl SettingsStore for PostgresSettings {
    async fn list(&self, tenant_id: TenantId) -> Result<Vec<SettingValue>, SettingsStoreError> {
        let docs = self
            .fetch(&tenant_id.to_string())
            .await
            .map_err(|error| SettingsStoreError::new(error.to_string()))?;
        docs.iter()
            .map(|doc| {
                serde_json::from_str(doc).map_err(|error| {
                    SettingsStoreError::new(format!(
                        "a stored setting could not be decoded: {error}"
                    ))
                })
            })
            .collect()
    }

    async fn put(
        &self,
        tenant_id: TenantId,
        value: &SettingValue,
    ) -> Result<(), SettingsStoreError> {
        let json = serde_json::to_string(value).map_err(|error| {
            SettingsStoreError::new(format!("could not serialize a setting: {error}"))
        })?;
        let slot = SettingSlot {
            setting_key: &value.setting_key,
            scope: value.scope.as_wire(),
            scope_id: &value.scope_id,
        };
        self.upsert(&tenant_id.to_string(), slot, &json)
            .await
            .map_err(|error| SettingsStoreError::new(error.to_string()))
    }

    async fn delete(
        &self,
        tenant_id: TenantId,
        setting_key: &str,
        scope: SettingScope,
        scope_id: &str,
    ) -> Result<bool, SettingsStoreError> {
        let slot = SettingSlot {
            setting_key,
            scope: scope.as_wire(),
            scope_id,
        };
        PostgresSettings::delete(self, &tenant_id.to_string(), slot)
            .await
            .map_err(|error| SettingsStoreError::new(error.to_string()))
    }
}

impl crate::data_migrations::DataMigrations for PostgresDataMigrations {
    async fn recorded_time(
        &self,
        name: &str,
    ) -> Result<Option<Timestamp>, crate::data_migrations::DataMigrationError> {
        let unavailable = |error: String| crate::data_migrations::DataMigrationError::new(error);
        Self::recorded_time(self, name)
            .await
            .map_err(|error| unavailable(error.to_string()))?
            .map(|ms| {
                Timestamp::from_milliseconds_since_epoch(ms)
                    .map_err(|error| unavailable(error.to_string()))
            })
            .transpose()
    }

    async fn record(
        &self,
        name: &str,
        at: Timestamp,
    ) -> Result<(), crate::data_migrations::DataMigrationError> {
        Self::record(self, name, at.as_milliseconds_since_epoch())
            .await
            .map_err(|error| crate::data_migrations::DataMigrationError::new(error.to_string()))
    }
}
