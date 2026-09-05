// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Wiring the cloud's persistence seams to their `store-postgres` tables.
//!
//! The `RollupStore` ([ADR-0036](../../../docs/adr/0036-materialised-rollups.md)), `ApiKeyStore`
//! ([ADR-0037](../../../docs/adr/0037-api-keys.md)), `AdminStore`
//! ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)), `ConfigTreeStore`
//! ([ADR-0033](../../../docs/adr/0033-config-tree.md)), `SubjectStore`
//! ([ADR-0035](../../../docs/adr/0035-retention-and-pii-masking.md)), `WebhookEndpointStore`
//! ([ADR-0032](../../../docs/adr/0032-webhooks.md)), `ReconcileStore`
//! ([ADR-0040](../../../docs/adr/0040-reconciliation.md)), `DeviceProposalStore`
//! ([ADR-0041](../../../docs/adr/0041-device-onboarding.md)), `TranslationStore`
//! ([ADR-0043](../../../docs/adr/0043-translation-grid.md)) and `ActivationCodeStore`
//! ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)) traits live here in the cloud, where the
//! handlers that consume them are; the Postgres tables behind them live in `store-postgres`, the
//! cloud's one Postgres adapter ([ADR-0016](../../../docs/adr/0016-postgres-access.md)). This module
//! is the thin seam between the two: it implements each cloud trait for the adapter's query type,
//! turning the plain row the adapter returns into the cloud's domain shape. All SQL stays in the
//! adapter; all domain conversion stays here — the adapter never learns a cloud type, and the cloud
//! never writes SQL.

use std::collections::{BTreeMap, HashSet};

use store_postgres::{
    AdminInviteRow, AdminSessionRow, AdminUserRow, AlertRow, AreaRow, AssignmentRow, AuditLogRow,
    AuditOrder, BrandRow, CampaignRow, CatalogItemRow, CatalogLayoutButtonRow, CatalogMenuRow,
    CatalogMenuSectionRow, CatalogModifierGroupRow, CatalogPlacementRow, CatalogTaxClassRow,
    CatalogTaxonomyRow, DeviceRow, EmployeeOrder, EmployeeRow, FleetStoreRow, InventoryRow,
    ItemOrder, MediaAssetRow, NewScheduledPublishRow, NewSessionRow, NewVoucherRow, OrderQueueRow,
    PendingOrderRow, PostgresActivationCodes, PostgresAdmin, PostgresAlerts, PostgresApiKeys,
    PostgresAudit, PostgresCampaigns, PostgresCatalog, PostgresConfigTrees,
    PostgresDeviceProposals, PostgresFleet, PostgresFloor, PostgresInventory, PostgresMedia,
    PostgresOrderQueue, PostgresPeople, PostgresReconcile, PostgresRegistry, PostgresReleases,
    PostgresRollups, PostgresScheduledPublishes, PostgresStore, PostgresStoreDirectory,
    PostgresSubjects, PostgresTaskHealth, PostgresTaxRates, PostgresTranslations, PostgresVouchers,
    PostgresWebhooks, ReleaseArtifactRow, RoleTemplateRow, RoutingRuleRow, RowUpdate,
    ScheduledPublishRow, StationRow, StoreRow, TableRow, TaskHealthRow, TaxRateRow, TenantRow,
    VoucherRow,
};

use pos_ports::PortError;
use pos_ports::dynamic::BoxFuture;
use pos_proto::campaign::PublishedCampaign;
use pos_proto::devices::DeviceConnection;
use pos_proto::display::GridPosition;
use pos_proto::enums::SalesChannel;
use pos_proto::ids::{
    AreaId, CampaignId, ConfigVersionId, CourseId, DeviceId, DisplayCategoryId,
    DisplaySubcategoryId, EventId, IngredientId, MenuItemId, StationId, StoreId, SubjectId,
    SupplierId, TableId, TaxClassId, TenantId,
};
use pos_proto::inventory::{PublishedIngredient, PublishedRecipe, PublishedSupplier};
use pos_proto::locale::{TaxComponent, TaxRate};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::{Open, WireEnum};

use pos_core::activation::CodeStatus;

use crate::activation::{ActivationCodeStore, ActivationStoreError, DeviceCredential, IssuedCode};
use crate::alerts::{AlertKind, AlertRecord, AlertSeverity, AlertStore, AlertStoreError};
use crate::audit::{
    AuditActor, AuditEntry, AuditId, AuditQuery, AuditStore, AuditStoreError, TrailOrder,
};
use crate::auth::SuperAdminCredential;
use crate::auth::admin::{
    AdminCredential, AdminInvite, AdminRole, AdminStatus, AdminStore, AdminStoreError, AdminUser,
    LiveSession, NewAdminInvite, NewAdminSession, NewAdminUser, NewRecoveryCode, SessionSummary,
};
use crate::auth::apikey::{
    ApiKeyAdminStore, ApiKeyId, ApiKeyStore, ApiKeyStoreError, ApiKeySummary, StoredApiKey,
};
use crate::auth::totp::TotpSecret;
use crate::campaigns::{CampaignStore, CampaignStoreError};
use crate::catalog::{
    CatalogItem, CatalogStore, CatalogStoreError, ChannelPrice, DisplayCategory,
    DisplaySubcategory, ItemCategory, ItemCategoryId, ItemListFilter, ItemSort, ItemSubcategory,
    ItemSubcategoryId, LayoutButton, Menu, MenuId, MenuPlacement, MenuSection, MenuSectionId,
    ModifierGroup, ModifierGroupId, TaxClass,
};
use crate::cloud::{StoreOwner, StoreOwners};
use crate::config_tree::{ConfigStoreError, ConfigTreeState, ConfigTreeStore};
use crate::dashboard::projection::{RollupError, RollupStore, StoredRollups};
use crate::dashboard::projector::StoreCatalog;
use crate::devices::{
    DeviceProposalError, DeviceProposalId, DeviceProposalStatus, DeviceProposalStore,
    DeviceProposalSummary, PersistedDeviceProposal,
};
use pos_core::lease::LeaseGeneration;

use crate::fleet::{FleetRow, FleetStore, FleetStoreError, OtaReportStore};
use crate::floorplan::{
    Area, AreaStore, AreaUpdate, FloorStoreError, NewArea, NewRoutingRule, NewStation, NewTable,
    RoutingRule, RoutingRuleId, RoutingRuleStore, Station, StationStore, StationUpdate, Table,
    TableStore, TableUpdate,
};
use crate::health::{TaskHealth, TaskHealthError, TaskHealthStore};
use crate::inventory::{InventoryStore, InventoryStoreError};
use crate::lease::{LeaseStore, LeaseStoreError};
use crate::media::{MediaId, MediaStore, MediaStoreError, MediaSummary, NewMediaAsset, Rendition};
use crate::orders::StoreDirectory;
use crate::ota::{
    RecordOutcome, ReleaseArtifact, ReleaseStore, ReleaseStoreError, TargetTriple, admit_artifact,
};
use crate::paging::{Page, PageRequest};
use crate::people::{
    Assignment, AssignmentId, AssignmentStore, AssignmentStoreError, Employee, EmployeeId,
    EmployeeListFilter, EmployeeSort, EmployeeStore, EmployeeStoreError, EmployeeUpdate,
    NewAssignment, NewEmployee, NewRoleTemplate, RoleTemplate, RoleTemplateId, RoleTemplateStore,
    RoleTemplateStoreError, RoleTemplateUpdate,
};
use crate::reconcile::{ReconcileError, ReconcileRun, ReconcileRunStore, ReconcileStore};
use crate::registry::{
    BrandId, BrandRecord, DeviceRecord, EntityStatus, RegistryStore, RegistryStoreError,
    StoreRecord, TenantRecord,
};
use crate::relay::{
    OrderQueueId, OrderQueueStore, OrderRecord, OrderStatus, PendingOrder, QueuedOrderPayload,
    StoreOutcome,
};
use crate::retention::{RetentionError, SubjectRecord, SubjectStore};
use crate::scheduling::{
    NewScheduledPublish, ScheduledPublish, ScheduledPublishError, ScheduledPublishStatus,
    ScheduledPublishStore,
};
use crate::tax::{TaxRateEntry, TaxRateStore, TaxRateStoreError};
use crate::translations::{TranslationGrid, TranslationStore, TranslationStoreError};
use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned};
use crate::vouchers::{NewVoucher, VoucherRecord, VoucherStatus, VoucherStore, VoucherStoreError};
use crate::webhook::sign::SigningSecret;
use crate::webhook::store::{
    PersistedWebhook, WebhookEndpointId, WebhookEndpointStore, WebhookStoreError, WebhookSummary,
};

impl RollupStore for PostgresRollups {
    async fn load(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<StoredRollups, RollupError> {
        match self
            .load_state(tenant, store_id)
            .await
            .map_err(|error| RollupError::store(error.to_string()))?
        {
            Some(json) => serde_json::from_str(&json).map_err(|error| {
                RollupError::store(format!("decoding the stored rollup state failed: {error}"))
            }),
            // A store with no row yet reads as an empty, cursor-less rollup — the same default the
            // projector starts from, so a first read and a first projection agree.
            None => Ok(StoredRollups::default()),
        }
    }

    async fn save(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        rollups: &StoredRollups,
    ) -> Result<(), RollupError> {
        let json = serde_json::to_string(rollups).map_err(|error| {
            RollupError::store(format!("encoding the rollup state failed: {error}"))
        })?;
        self.save_state(tenant, store_id, &json)
            .await
            .map_err(|error| RollupError::store(error.to_string()))
    }
}

impl StoreCatalog for PostgresStore {
    async fn active_stores(&self) -> Result<Vec<(TenantId, StoreId)>, PortError> {
        self.list_active_stores().await
    }
}

impl SubjectStore for PostgresSubjects {
    async fn due_before(
        &self,
        cutoff: Timestamp,
        limit: u32,
    ) -> Result<Vec<SubjectRecord>, RetentionError> {
        let rows = self
            .fetch_due(cutoff.as_milliseconds_since_epoch(), i64::from(limit))
            .await
            .map_err(|error| RetentionError::new(error.to_string()))?;
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let subject_id = row
                .subject_id
                .parse::<Ulid>()
                .map(SubjectId::new)
                .map_err(|_| {
                    RetentionError::new(format!("subject id is not a ULID: {}", row.subject_id))
                })?;
            let collected_at = Timestamp::from_milliseconds_since_epoch(row.collected_at_ms)
                .map_err(|_| RetentionError::new("a subject's collected_at is out of range"))?;
            let fields: BTreeMap<String, String> =
                serde_json::from_str(&row.fields_json).map_err(|error| {
                    RetentionError::new(format!("decoding a subject's fields failed: {error}"))
                })?;
            // `fetch_due` returns only unmasked rows, so `masked_at` is `None` by construction.
            records.push(SubjectRecord {
                subject_id,
                collected_at,
                fields,
                masked_at: None,
            });
        }
        Ok(records)
    }

    async fn save_masked(&self, records: &[SubjectRecord]) -> Result<u64, RetentionError> {
        let mut saved: u64 = 0;
        for record in records {
            // A record handed here has been through `SubjectRecord::masked`, so `masked_at` is set; a
            // record without it is not a masking to write, so skip it rather than stamp a guess.
            let Some(masked_at) = record.masked_at else {
                continue;
            };
            let fields_json = serde_json::to_string(&record.fields).map_err(|error| {
                RetentionError::new(format!(
                    "encoding a masked subject's fields failed: {error}"
                ))
            })?;
            let updated = self
                .mask(
                    &record.subject_id.to_string(),
                    &fields_json,
                    masked_at.as_milliseconds_since_epoch(),
                )
                .await
                .map_err(|error| RetentionError::new(error.to_string()))?;
            if updated {
                saved = saved.saturating_add(1);
            }
        }
        Ok(saved)
    }

    async fn fetch(
        &self,
        tenant: TenantId,
        subject_id: SubjectId,
    ) -> Result<Option<SubjectRecord>, RetentionError> {
        let Some(row) = self
            .fetch_one(&subject_id.to_string(), &tenant.to_string())
            .await
            .map_err(|error| RetentionError::new(error.to_string()))?
        else {
            return Ok(None);
        };
        let subject_id = row
            .subject_id
            .parse::<Ulid>()
            .map(SubjectId::new)
            .map_err(|_| {
                RetentionError::new(format!("subject id is not a ULID: {}", row.subject_id))
            })?;
        let collected_at = Timestamp::from_milliseconds_since_epoch(row.collected_at_ms)
            .map_err(|_| RetentionError::new("a subject's collected_at is out of range"))?;
        let masked_at = match row.masked_at_ms {
            Some(ms) => Some(
                Timestamp::from_milliseconds_since_epoch(ms)
                    .map_err(|_| RetentionError::new("a subject's masked_at is out of range"))?,
            ),
            None => None,
        };
        let fields: BTreeMap<String, String> =
            serde_json::from_str(&row.fields_json).map_err(|error| {
                RetentionError::new(format!("decoding a subject's fields failed: {error}"))
            })?;
        Ok(Some(SubjectRecord {
            subject_id,
            collected_at,
            fields,
            masked_at,
        }))
    }
}

impl WebhookEndpointStore for PostgresWebhooks {
    async fn insert(&self, endpoint: &PersistedWebhook) -> Result<(), WebhookStoreError> {
        // A freshly registered endpoint always has no cursor and is enabled; the adapter's `create`
        // defaults those, so only the durable identity + destination + secret cross here.
        self.create(
            &endpoint.id.to_string(),
            &endpoint.tenant_id.to_string(),
            &endpoint.store_id.to_string(),
            &endpoint.url,
            endpoint.secret.expose_secret(),
        )
        .await
        .map_err(|error| WebhookStoreError::new(error.to_string()))
    }

    async fn list_for_tenant(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<WebhookSummary>, WebhookStoreError> {
        let rows = self
            .list(&tenant_id.to_string())
            .await
            .map_err(|error| WebhookStoreError::new(error.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|row| WebhookSummary {
                id: row.id,
                store_id: row.store_id,
                url: row.url,
                cursor: row.cursor,
                disabled: row.disabled,
            })
            .collect())
    }

    async fn delete(
        &self,
        tenant_id: TenantId,
        id: WebhookEndpointId,
    ) -> Result<bool, WebhookStoreError> {
        self.remove(&tenant_id.to_string(), &id.to_string())
            .await
            .map_err(|error| WebhookStoreError::new(error.to_string()))
    }

    async fn load_enabled(&self) -> Result<Vec<PersistedWebhook>, WebhookStoreError> {
        let rows = self
            .fetch_enabled()
            .await
            .map_err(|error| WebhookStoreError::new(error.to_string()))?;
        let mut endpoints = Vec::with_capacity(rows.len());
        for row in rows {
            let id = row
                .id
                .parse::<Ulid>()
                .map(WebhookEndpointId::new)
                .map_err(|_| {
                    WebhookStoreError::new(format!("endpoint id is not a ULID: {}", row.id))
                })?;
            let tenant_id = row
                .tenant_id
                .parse::<TenantId>()
                .map_err(|_| WebhookStoreError::new("a webhook tenant id is not a ULID"))?;
            let store_id = row
                .store_id
                .parse::<StoreId>()
                .map_err(|_| WebhookStoreError::new("a webhook store id is not a ULID"))?;
            let cursor = match row.cursor {
                Some(text) => Some(text.parse::<EventId>().map_err(|_| {
                    WebhookStoreError::new("a webhook cursor is not an event-id ULID")
                })?),
                None => None,
            };
            endpoints.push(PersistedWebhook {
                id,
                tenant_id,
                store_id,
                url: row.url,
                secret: SigningSecret::new(row.secret),
                cursor,
                // `fetch_enabled` returns only enabled rows.
                disabled: false,
            });
        }
        Ok(endpoints)
    }

    async fn save_cursor(
        &self,
        id: WebhookEndpointId,
        cursor: EventId,
    ) -> Result<(), WebhookStoreError> {
        self.advance_cursor(&id.to_string(), &cursor.to_string())
            .await
            .map_err(|error| WebhookStoreError::new(error.to_string()))
    }

    async fn set_disabled(
        &self,
        id: WebhookEndpointId,
        disabled: bool,
    ) -> Result<(), WebhookStoreError> {
        self.mark_disabled(&id.to_string(), disabled)
            .await
            .map_err(|error| WebhookStoreError::new(error.to_string()))
    }
}

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
    ) -> Result<(), ConfigStoreError> {
        // A depth past `i64::MAX` is not reachable from a store's log, but saturating beats a panic
        // and beats dropping the heartbeat: the column is `bigint`, so this is the widest it holds.
        let depth = outbox_depth.map(|depth| i64::try_from(depth).unwrap_or(i64::MAX));
        // Same rule for the generation, and just as unreachable: a store issues a handful of leases
        // in its life, and `LeaseGeneration::next` saturates rather than wrapping (ADR-0049).
        let generation = lease_generation.map(|value| i64::try_from(value).unwrap_or(i64::MAX));
        self.record_heartbeat(
            tenant,
            store,
            seen_at.as_milliseconds_since_epoch(),
            depth,
            generation,
        )
        .await
        .map_err(|error| ConfigStoreError::new(error.to_string()))
    }
}

impl LeaseStore for PostgresConfigTrees {
    /// Forwards to the adapter's single-statement bump, so two admins replacing a machine at once
    /// serialise on the row rather than racing to the same generation.
    async fn bump(
        &self,
        tenant: TenantId,
        store: StoreId,
        issued_at: Timestamp,
    ) -> Result<LeaseGeneration, LeaseStoreError> {
        let generation = self
            .bump_store_lease(tenant, store, issued_at.as_milliseconds_since_epoch())
            .await
            .map_err(|error| LeaseStoreError::new(error.to_string()))?;
        stored_lease_generation(generation)
    }

    async fn current(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> Result<Option<LeaseGeneration>, LeaseStoreError> {
        let stored = self
            .store_lease(tenant, store)
            .await
            .map_err(|error| LeaseStoreError::new(error.to_string()))?;
        stored.map(stored_lease_generation).transpose()
    }
}

/// Reads a stored generation back into the domain's `u64`.
///
/// `bigint` is signed and this cloud is the only writer, so a negative value cannot have been issued
/// — it is a tampered or corrupt row. Refusing it beats clamping: generation `0` is a store's real
/// *first* lease, so a silent clamp would tell every box in the store it had been superseded.
fn stored_lease_generation(value: i64) -> Result<LeaseGeneration, LeaseStoreError> {
    u64::try_from(value)
        .map(LeaseGeneration::new)
        .map_err(|error| {
            LeaseStoreError::new(format!(
                "the stored lease generation is negative, which this cloud never writes: {error}"
            ))
        })
}

impl OtaReportStore for PostgresConfigTrees {
    async fn record_report(
        &self,
        tenant: TenantId,
        store: StoreId,
        installed: &str,
        self_test_passed: Option<bool>,
        reported_at: Timestamp,
    ) -> Result<(), FleetStoreError> {
        self.record_ota_report(
            tenant,
            store,
            installed,
            self_test_passed,
            reported_at.as_milliseconds_since_epoch(),
        )
        .await
        .map_err(|error| FleetStoreError::new(error.to_string()))
    }
}

impl AdminStore for PostgresAdmin {
    async fn load_credential(&self) -> Result<Option<AdminCredential>, AdminStoreError> {
        let row = self
            .fetch_credential()
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        Ok(row.map(|row| AdminCredential {
            credential: SuperAdminCredential::new(
                row.password_phc,
                TotpSecret::new(row.totp_secret),
            ),
            // A stored step is always a value this adapter itself wrote, so it fits u64; a stray
            // negative reads as "never used" rather than failing the load.
            last_used_totp_step: row
                .last_used_totp_step
                .and_then(|step| u64::try_from(step).ok()),
        }))
    }

    async fn provision_credential(
        &self,
        password_phc: String,
        totp_secret: Vec<u8>,
    ) -> Result<bool, AdminStoreError> {
        self.insert_credential(&password_phc, &totp_secret)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn record_totp_step(&self, step: u64) -> Result<(), AdminStoreError> {
        let step = i64::try_from(step).unwrap_or(i64::MAX);
        self.advance_totp_step(step)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn create_session(&self, session: NewAdminSession) -> Result<(), AdminStoreError> {
        self.insert_session(NewSessionRow {
            token_hash: &session.token_hash,
            created_at_ms: session.created_at.as_milliseconds_since_epoch(),
            expires_at_ms: session.expires_at.as_milliseconds_since_epoch(),
            absolute_expires_at_ms: Some(session.absolute_expires_at.as_milliseconds_since_epoch()),
            idle_ttl_ms: Some(session.idle_ttl_ms),
            admin_id: session.admin_id.as_deref(),
            ip: session.ip.as_deref(),
            user_agent: session.user_agent.as_deref(),
        })
        .await
        .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn session_is_valid(
        &self,
        token_hash: [u8; 32],
        now: Timestamp,
    ) -> Result<bool, AdminStoreError> {
        self.session_valid(&token_hash, now.as_milliseconds_since_epoch())
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn session_admin(
        &self,
        token_hash: [u8; 32],
        now: Timestamp,
    ) -> Result<Option<LiveSession>, AdminStoreError> {
        let found = self
            .fetch_session_admin(&token_hash, now.as_milliseconds_since_epoch())
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        Ok(found.map(|admin_id| LiveSession { admin_id }))
    }

    async fn revoke_session(&self, token_hash: [u8; 32]) -> Result<(), AdminStoreError> {
        self.delete_session(&token_hash)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn list_admin_sessions(
        &self,
        admin_id: &str,
        now: Timestamp,
    ) -> Result<Vec<SessionSummary>, AdminStoreError> {
        let rows =
            PostgresAdmin::list_admin_sessions(self, admin_id, now.as_milliseconds_since_epoch())
                .await
                .map_err(|error| AdminStoreError::new(error.to_string()))?;
        rows.into_iter().map(session_summary_from_row).collect()
    }

    async fn revoke_admin_session(
        &self,
        admin_id: &str,
        token_hash: [u8; 32],
    ) -> Result<bool, AdminStoreError> {
        self.delete_admin_session(admin_id, &token_hash)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn revoke_other_admin_sessions(
        &self,
        admin_id: &str,
        except_token_hash: [u8; 32],
    ) -> Result<u64, AdminStoreError> {
        self.delete_other_admin_sessions(admin_id, &except_token_hash)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn rotate_totp_secret(&self, secret: Vec<u8>) -> Result<(), AdminStoreError> {
        PostgresAdmin::rotate_totp_secret(self, &secret)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn store_recovery_codes(
        &self,
        admin_id: &str,
        codes: Vec<NewRecoveryCode>,
    ) -> Result<(), AdminStoreError> {
        let codes: Vec<(String, Vec<u8>)> = codes
            .into_iter()
            .map(|code| (code.id, code.code_hash.to_vec()))
            .collect();
        self.replace_recovery_codes(admin_id, &codes)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn consume_recovery_code(
        &self,
        admin_id: &str,
        code_hash: [u8; 32],
        now: Timestamp,
    ) -> Result<bool, AdminStoreError> {
        PostgresAdmin::consume_recovery_code(
            self,
            admin_id,
            &code_hash,
            now.as_milliseconds_since_epoch(),
        )
        .await
        .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn count_recovery_codes(&self, admin_id: &str) -> Result<u64, AdminStoreError> {
        let count = PostgresAdmin::count_recovery_codes(self, admin_id)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    async fn create_admin_user(&self, user: NewAdminUser) -> Result<bool, AdminStoreError> {
        self.insert_admin_user(
            &user.id,
            &user.email,
            &user.name,
            user.role.as_token(),
            AdminStatus::Active.as_token(),
            &user.password_phc,
            &user.totp_secret,
        )
        .await
        .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn list_admin_users(&self) -> Result<Vec<AdminUser>, AdminStoreError> {
        let rows = PostgresAdmin::list_admin_users(self)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        rows.into_iter().map(admin_user_from_row).collect()
    }

    async fn get_admin_user(&self, id: &str) -> Result<Option<AdminUser>, AdminStoreError> {
        let row = self
            .fetch_admin_user(id)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        row.map(admin_user_from_row).transpose()
    }

    async fn find_admin_user_by_email(
        &self,
        email: &str,
    ) -> Result<Option<AdminUser>, AdminStoreError> {
        let row = self
            .fetch_admin_user_by_email(email)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        row.map(admin_user_from_row).transpose()
    }

    async fn set_admin_user_role(
        &self,
        id: &str,
        role: AdminRole,
    ) -> Result<bool, AdminStoreError> {
        PostgresAdmin::set_admin_user_role(self, id, role.as_token())
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn set_admin_user_status(
        &self,
        id: &str,
        status: AdminStatus,
    ) -> Result<bool, AdminStoreError> {
        PostgresAdmin::set_admin_user_status(self, id, status.as_token())
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn count_active_owners(&self) -> Result<u64, AdminStoreError> {
        let count = PostgresAdmin::count_active_owners(self)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    async fn create_invite(&self, invite: NewAdminInvite) -> Result<(), AdminStoreError> {
        self.insert_invite(
            &invite.id,
            &invite.email,
            &invite.name,
            invite.role.as_token(),
            &invite.token_hash,
            &invite.invited_by,
            invite.expires_at.as_milliseconds_since_epoch(),
        )
        .await
        .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn find_pending_invite_by_token(
        &self,
        token_hash: [u8; 32],
        now: Timestamp,
    ) -> Result<Option<AdminInvite>, AdminStoreError> {
        let row = self
            .fetch_pending_invite_by_token(&token_hash, now.as_milliseconds_since_epoch())
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        row.map(admin_invite_from_row).transpose()
    }

    async fn mark_invite_accepted(
        &self,
        id: &str,
        accepted_at: Timestamp,
    ) -> Result<bool, AdminStoreError> {
        PostgresAdmin::mark_invite_accepted(self, id, accepted_at.as_milliseconds_since_epoch())
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }

    async fn list_pending_invites(
        &self,
        now: Timestamp,
    ) -> Result<Vec<AdminInvite>, AdminStoreError> {
        let rows = PostgresAdmin::list_pending_invites(self, now.as_milliseconds_since_epoch())
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))?;
        rows.into_iter().map(admin_invite_from_row).collect()
    }

    async fn revoke_invite(&self, id: &str) -> Result<bool, AdminStoreError> {
        self.delete_pending_invite(id)
            .await
            .map_err(|error| AdminStoreError::new(error.to_string()))
    }
}

/// Converts a stored `admin_users` row into the domain [`AdminUser`], failing loudly if the role or
/// status token is unrecognised — the table's CHECK constraints keep them within the known
/// vocabularies, so an unparseable value is store corruption, not an ordinary absence.
fn admin_user_from_row(row: AdminUserRow) -> Result<AdminUser, AdminStoreError> {
    let role = AdminRole::from_token(&row.role)
        .ok_or_else(|| AdminStoreError::new(format!("unknown admin role token: {}", row.role)))?;
    let status = AdminStatus::from_token(&row.status).ok_or_else(|| {
        AdminStoreError::new(format!("unknown admin status token: {}", row.status))
    })?;
    Ok(AdminUser {
        id: row.id,
        email: row.email,
        name: row.name,
        role,
        status,
    })
}

/// Converts a stored `admin_sessions` row into the domain [`SessionSummary`], failing loudly if the
/// stored token hash is not 32 bytes or a timestamp is out of range — either is store corruption, not
/// an ordinary absence.
fn session_summary_from_row(row: AdminSessionRow) -> Result<SessionSummary, AdminStoreError> {
    let token_hash: [u8; 32] = row.token_hash.try_into().map_err(|_ignored: Vec<u8>| {
        AdminStoreError::new("a stored session token hash is not 32 bytes")
    })?;
    let created_at = Timestamp::from_milliseconds_since_epoch(row.created_at_ms)
        .map_err(|_ignored| AdminStoreError::new("a session created_at is out of range"))?;
    let expires_at = Timestamp::from_milliseconds_since_epoch(row.expires_at_ms)
        .map_err(|_ignored| AdminStoreError::new("a session expires_at is out of range"))?;
    Ok(SessionSummary {
        token_hash,
        ip: row.ip,
        user_agent: row.user_agent,
        created_at,
        expires_at,
    })
}

/// Converts a stored `admin_invites` row into the domain [`AdminInvite`], failing loudly on an
/// unrecognised role token (the table's CHECK keeps it within the vocabulary).
fn admin_invite_from_row(row: AdminInviteRow) -> Result<AdminInvite, AdminStoreError> {
    let role = AdminRole::from_token(&row.role)
        .ok_or_else(|| AdminStoreError::new(format!("unknown admin role token: {}", row.role)))?;
    Ok(AdminInvite {
        id: row.id,
        email: row.email,
        name: row.name,
        role,
        invited_by: row.invited_by,
        accepted: row.accepted,
    })
}

impl ApiKeyAdminStore for PostgresApiKeys {
    async fn insert(&self, key: &StoredApiKey) -> Result<(), ApiKeyStoreError> {
        let store_id = key.store_id.map(|id| id.to_string());
        PostgresApiKeys::insert(
            self,
            &key.id.to_string(),
            &key.tenant_id.to_string(),
            store_id.as_deref(),
            &key.secret_hash(),
            &key.scope_wire_names(),
            key.expires_at_ms(),
        )
        .await
        .map_err(|error| ApiKeyStoreError::new(error.to_string()))
    }

    async fn list_for_tenant(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<ApiKeySummary>, ApiKeyStoreError> {
        let rows = PostgresApiKeys::list_for_tenant(self, &tenant_id.to_string())
            .await
            .map_err(|error| ApiKeyStoreError::new(error.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|row| ApiKeySummary {
                id: row.id,
                store_id: row.store_id,
                scopes: row.scopes,
                revoked: row.revoked,
                expires_at_ms: row.expires_at_ms,
            })
            .collect())
    }

    async fn revoke(&self, id: ApiKeyId) -> Result<bool, ApiKeyStoreError> {
        PostgresApiKeys::revoke(self, &id.to_string())
            .await
            .map_err(|error| ApiKeyStoreError::new(error.to_string()))
    }
}

impl ApiKeyStore for PostgresApiKeys {
    async fn lookup(&self, id: ApiKeyId) -> Result<Option<StoredApiKey>, ApiKeyStoreError> {
        let row = self
            .fetch(&id.to_string())
            .await
            .map_err(|error| ApiKeyStoreError::new(error.to_string()))?;
        match row {
            Some(row) => {
                let stored = StoredApiKey::from_parts(
                    id,
                    &row.tenant_id,
                    row.store_id.as_deref(),
                    &row.secret_hash,
                    &row.scopes,
                    row.revoked,
                    row.expires_at_ms,
                )
                .map_err(ApiKeyStoreError::new)?;
                Ok(Some(stored))
            }
            None => Ok(None),
        }
    }
}

impl ActivationCodeStore for PostgresActivationCodes {
    async fn issue(
        &self,
        code_hash: [u8; 32],
        tenant_id: TenantId,
        store_id: StoreId,
        device_id: DeviceId,
    ) -> Result<(), ActivationStoreError> {
        self.issue(
            &code_hash,
            &tenant_id.to_string(),
            &store_id.to_string(),
            &device_id.to_string(),
        )
        .await
        .map_err(|error| ActivationStoreError::new(error.to_string()))
    }

    async fn lookup(
        &self,
        code_hash: [u8; 32],
    ) -> Result<Option<IssuedCode>, ActivationStoreError> {
        let Some(row) = self
            .lookup(&code_hash)
            .await
            .map_err(|error| ActivationStoreError::new(error.to_string()))?
        else {
            return Ok(None);
        };
        let (Ok(tenant_id), Ok(store_id), Ok(device_id)) = (
            row.tenant_id.parse::<Ulid>().map(TenantId::new),
            row.store_id.parse::<Ulid>().map(StoreId::new),
            row.device_id.parse::<Ulid>().map(DeviceId::new),
        ) else {
            return Err(ActivationStoreError::new(
                "an activation-code row holds a non-ULID id",
            ));
        };
        let status = match row.status.as_str() {
            "issued" => CodeStatus::Issued,
            "redeemed" => CodeStatus::Redeemed,
            "revoked" => CodeStatus::Revoked,
            other => {
                return Err(ActivationStoreError::new(format!(
                    "an activation-code row holds an unknown status {other}"
                )));
            }
        };
        Ok(Some(IssuedCode {
            tenant_id,
            store_id,
            device_id,
            status,
        }))
    }

    async fn consume_and_provision(
        &self,
        code_hash: [u8; 32],
        credential: &DeviceCredential,
    ) -> Result<bool, ActivationStoreError> {
        let credential_id = credential.id.to_string();
        let secret_hash = credential.secret_hash();
        self.consume_and_provision(&code_hash, &credential_id, &secret_hash)
            .await
            .map_err(|error| ActivationStoreError::new(error.to_string()))
    }

    async fn revoke_slot(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
        device_id: DeviceId,
    ) -> Result<u64, ActivationStoreError> {
        self.revoke_slot(
            &tenant_id.to_string(),
            &store_id.to_string(),
            &device_id.to_string(),
        )
        .await
        .map_err(|error| ActivationStoreError::new(error.to_string()))
    }
}

impl ReconcileStore for PostgresReconcile {
    async fn absent_event_ids(
        &self,
        tenant: TenantId,
        store: StoreId,
        candidates: &[EventId],
    ) -> Result<Vec<EventId>, ReconcileError> {
        let candidate_strings: Vec<String> = candidates.iter().map(ToString::to_string).collect();
        let present: HashSet<String> = self
            .present_event_ids(&tenant.to_string(), &store.to_string(), &candidate_strings)
            .await
            .map_err(|error| ReconcileError::new(error.to_string()))?
            .into_iter()
            .collect();
        // The missing ids are the candidates the log did not return; return them from the caller's own
        // typed list, so no id is re-parsed from a string on the way back out.
        Ok(candidates
            .iter()
            .filter(|id| !present.contains(&id.to_string()))
            .copied()
            .collect())
    }
}

impl ReconcileRunStore for PostgresReconcile {
    async fn record_run(&self, tenant: TenantId, run: &ReconcileRun) -> Result<(), ReconcileError> {
        // Counts are bounded by a reconcile window; clamp the usize→i32 conversion defensively so a
        // pathological manifest records as i32::MAX rather than overflowing.
        let offered = i32::try_from(run.candidates_offered).unwrap_or(i32::MAX);
        let missing = i32::try_from(run.missing_found).unwrap_or(i32::MAX);
        self.record_reconcile_run(
            &run.run_id,
            &tenant.to_string(),
            &run.store.to_string(),
            offered,
            missing,
            run.ran_at.as_milliseconds_since_epoch(),
        )
        .await
        .map_err(|error| ReconcileError::new(error.to_string()))
    }

    async fn list_runs(
        &self,
        tenant: TenantId,
        store: Option<StoreId>,
        limit: u32,
    ) -> Result<Vec<ReconcileRun>, ReconcileError> {
        let store_string = store.map(|id| id.to_string());
        let rows = self
            .list_reconcile_runs(
                &tenant.to_string(),
                store_string.as_deref(),
                i64::from(limit),
            )
            .await
            .map_err(|error| ReconcileError::new(error.to_string()))?;
        rows.into_iter()
            .map(|row| {
                let store = row
                    .store_id
                    .parse::<Ulid>()
                    .map(StoreId::new)
                    .map_err(|_| {
                        ReconcileError::new("a stored reconcile-run store_id is not a ULID")
                    })?;
                let ran_at =
                    Timestamp::from_milliseconds_since_epoch(row.ran_at).map_err(|_| {
                        ReconcileError::new("a stored reconcile-run ran_at is out of range")
                    })?;
                Ok(ReconcileRun {
                    run_id: row.run_id,
                    store,
                    candidates_offered: u32::try_from(row.candidates_offered).unwrap_or(0),
                    missing_found: u32::try_from(row.missing_found).unwrap_or(0),
                    ran_at,
                })
            })
            .collect()
    }
}

impl DeviceProposalStore for PostgresDeviceProposals {
    async fn propose(&self, proposal: &PersistedDeviceProposal) -> Result<(), DeviceProposalError> {
        self.create(
            &proposal.id.to_string(),
            &proposal.tenant_id.to_string(),
            &proposal.store_id.to_string(),
            proposal.kind.as_wire(),
            &proposal.name,
            &proposal.address,
        )
        .await
        .map_err(|error| DeviceProposalError::new(error.to_string()))
    }

    async fn list(
        &self,
        tenant: TenantId,
        store: Option<StoreId>,
        status: DeviceProposalStatus,
    ) -> Result<Vec<DeviceProposalSummary>, DeviceProposalError> {
        let store = store.map(|id| id.to_string());
        let rows = self
            .fetch(&tenant.to_string(), store.as_deref(), status.as_wire())
            .await
            .map_err(|error| DeviceProposalError::new(error.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|row| DeviceProposalSummary {
                id: row.id,
                store_id: row.store_id,
                kind: row.kind,
                name: row.name,
                address: row.address,
                connection: row.connection,
                station_id: row.station_id,
                status: row.status,
            })
            .collect())
    }

    async fn resolve(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        approved: bool,
        connection: Option<DeviceConnection>,
        station: Option<StationId>,
    ) -> Result<bool, DeviceProposalError> {
        let status = if approved {
            DeviceProposalStatus::Approved
        } else {
            DeviceProposalStatus::Rejected
        };
        let station = station.map(|id| id.to_string());
        self.mark(
            &tenant.to_string(),
            &id.to_string(),
            status.as_wire(),
            connection.map(DeviceConnection::as_wire),
            station.as_deref(),
        )
        .await
        .map_err(|error| DeviceProposalError::new(error.to_string()))
    }
}

impl TranslationStore for PostgresTranslations {
    async fn load(
        &self,
        tenant: TenantId,
    ) -> Result<Option<Versioned<TranslationGrid>>, TranslationStoreError> {
        let row = self
            .load_grid(&tenant.to_string())
            .await
            .map_err(|error| TranslationStoreError::new(error.to_string()))?;
        match row {
            Some((text, version)) => serde_json::from_str(&text)
                .map(|grid| Some(Versioned::new(grid, Version::new(version))))
                .map_err(|error| {
                    TranslationStoreError::new(format!("decoding the stored grid failed: {error}"))
                }),
            None => Ok(None),
        }
    }

    async fn save(
        &self,
        tenant: TenantId,
        grid: &TranslationGrid,
        expected: Option<&Version>,
    ) -> Result<UpdateOutcome, TranslationStoreError> {
        let json = serde_json::to_string(grid).map_err(|error| {
            TranslationStoreError::new(format!("encoding the grid failed: {error}"))
        })?;
        self.save_grid(&tenant.to_string(), &json, expected.map(Version::as_str))
            .await
            .map(update_outcome)
            .map_err(|error| TranslationStoreError::new(error.to_string()))
    }
}

// --- The order relay's queue and store directory (ADR-0061) ------------------------------------

/// Parses the adapter's `queued_id` text back to the cloud's [`OrderQueueId`].
fn parse_queued_id(text: &str) -> Result<OrderQueueId, PortError> {
    text.parse::<Ulid>()
        .map(OrderQueueId::new)
        .map_err(|_ignored| {
            PortError::internal(
                pos_ports::PortName::OrderIn,
                "a queued order id is not a ULID",
            )
        })
}

/// Turns the adapter's status/outcome columns into the cloud's [`OrderStatus`].
fn order_status(row: &OrderQueueRow) -> Result<OrderStatus, PortError> {
    if row.status == "reported" {
        let value = row.outcome.as_ref().ok_or_else(|| {
            PortError::internal(
                pos_ports::PortName::OrderIn,
                "a reported order is missing its stored outcome",
            )
        })?;
        let outcome: StoreOutcome = serde_json::from_value(value.clone()).map_err(|error| {
            PortError::internal(pos_ports::PortName::OrderIn, error.to_string())
        })?;
        Ok(OrderStatus::Reported(outcome))
    } else {
        Ok(OrderStatus::Pending)
    }
}

/// Turns one adapter row into the cloud's [`OrderRecord`].
fn order_record(row: &OrderQueueRow) -> Result<OrderRecord, PortError> {
    Ok(OrderRecord {
        queued_id: parse_queued_id(&row.queued_id)?,
        status: order_status(row)?,
    })
}

impl OrderQueueStore for PostgresOrderQueue {
    async fn enqueue(
        &self,
        tenant: TenantId,
        queued_id: OrderQueueId,
        payload: &QueuedOrderPayload,
    ) -> Result<OrderRecord, PortError> {
        let json = serde_json::to_value(payload).map_err(|error| {
            PortError::internal(pos_ports::PortName::OrderIn, error.to_string())
        })?;
        let row = self
            .enqueue(
                &tenant.to_string(),
                &payload.store_id,
                &payload.sales_channel,
                &payload.external_reference,
                &queued_id.to_string(),
                &json,
            )
            .await?;
        order_record(&row)
    }

    async fn outcome(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        sales_channel: &str,
        external_reference: &str,
    ) -> Result<Option<OrderRecord>, PortError> {
        let row = self
            .outcome(
                &tenant.to_string(),
                &store_id.to_string(),
                sales_channel,
                external_reference,
            )
            .await?;
        row.as_ref().map(order_record).transpose()
    }

    async fn pull_pending(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        limit: u32,
    ) -> Result<Vec<PendingOrder>, PortError> {
        let rows = self
            .pull_pending(&tenant.to_string(), &store_id.to_string(), i64::from(limit))
            .await?;
        rows.into_iter().map(pending_order).collect()
    }

    async fn record_outcome(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        queued_id: OrderQueueId,
        outcome: &StoreOutcome,
    ) -> Result<bool, PortError> {
        let json = serde_json::to_value(outcome).map_err(|error| {
            PortError::internal(pos_ports::PortName::OrderIn, error.to_string())
        })?;
        self.record_outcome(
            &tenant.to_string(),
            &store_id.to_string(),
            &queued_id.to_string(),
            &json,
        )
        .await
    }
}

/// Turns one pending adapter row into the cloud's [`PendingOrder`].
fn pending_order(row: PendingOrderRow) -> Result<PendingOrder, PortError> {
    let queued_id = parse_queued_id(&row.queued_id)?;
    let payload: QueuedOrderPayload = serde_json::from_value(row.payload)
        .map_err(|error| PortError::internal(pos_ports::PortName::OrderIn, error.to_string()))?;
    Ok(PendingOrder { queued_id, payload })
}

impl StoreOwners for PostgresStoreDirectory {
    /// Resolves the store's owner from the registry, for the stamp `Cloud::ingest` puts on every
    /// event ([ADR-0101](../../../docs/adr/0101-the-cloud-stamps-the-tenant.md)).
    ///
    /// A stored id that will not parse is treated as **unknown**, not as a nil owner: filing a
    /// store's events under a fabricated tenant is the failure this stamp exists to prevent, and the
    /// batch keeps its own claim while the warning names the row.
    fn owner_of(&self, store_id: StoreId) -> BoxFuture<'_, Result<Option<StoreOwner>, PortError>> {
        Box::pin(async move {
            let Some((tenant, brand)) =
                PostgresStoreDirectory::owner_of(self, &store_id.to_string()).await?
            else {
                return Ok(None);
            };
            let Ok(tenant_id) = tenant.parse::<Ulid>().map(TenantId::new) else {
                tracing::warn!(
                    store = %store_id,
                    "the registry's tenant id for this store is not a ULID"
                );
                return Ok(None);
            };
            // A store under no brand, and a brand id the registry cannot spell, both land on the nil
            // id — "no brand", which is the truth in the first case and the safe reading in the
            // second. Neither is a reason to refuse the tenant, which is the column that isolates.
            let brand_id = brand
                .and_then(|brand| brand.parse::<Ulid>().ok())
                .map_or_else(
                    pos_proto::ids::BrandId::default,
                    pos_proto::ids::BrandId::new,
                );
            Ok(Some(StoreOwner {
                tenant_id,
                brand_id,
            }))
        })
    }
}

impl StoreDirectory for PostgresStoreDirectory {
    async fn tenant_of(&self, store_id: StoreId) -> Result<Option<TenantId>, PortError> {
        let id = store_id.to_string();
        match self.tenant_of(&id).await? {
            Some(text) => text
                .parse::<Ulid>()
                .map(TenantId::new)
                .map(Some)
                .map_err(|_ignored| {
                    PortError::internal(
                        pos_ports::PortName::OrderIn,
                        "a stored tenant id is not a ULID",
                    )
                }),
            None => Ok(None),
        }
    }
}

// --- The org registry (ADR-0065) ---------------------------------------------------------------

fn parse_registry_tenant(text: &str) -> Result<TenantId, RegistryStoreError> {
    text.parse::<Ulid>().map(TenantId::new).map_err(|_ignored| {
        RegistryStoreError::new(format!("a registry tenant id is not a ULID: {text}"))
    })
}

fn parse_registry_brand(text: &str) -> Result<BrandId, RegistryStoreError> {
    text.parse::<Ulid>().map(BrandId::new).map_err(|_ignored| {
        RegistryStoreError::new(format!("a registry brand id is not a ULID: {text}"))
    })
}

fn parse_registry_store(text: &str) -> Result<StoreId, RegistryStoreError> {
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
fn update_outcome(update: RowUpdate) -> UpdateOutcome {
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
fn create_outcome(inserted: Option<String>) -> CreateOutcome {
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

// --- The fleet read model (ADR-0068) -----------------------------------------------------------

/// Converts one joined `store-postgres` fleet row into the cloud's [`FleetRow`]. A liveness timestamp
/// out of range (impossible for values this cloud wrote) fails safe to "never seen" rather than
/// failing the whole listing; a negative backlog (impossible from a `count`) reads as zero.
fn fleet_row(row: FleetStoreRow) -> Result<FleetRow, FleetStoreError> {
    let last_seen_at = row
        .last_seen_at_ms
        .and_then(|ms| Timestamp::from_milliseconds_since_epoch(ms).ok());
    let last_config_pull_at = row
        .last_config_pull_at_ms
        .and_then(|ms| Timestamp::from_milliseconds_since_epoch(ms).ok());
    let relay_oldest_pending_at = row
        .oldest_pending_at_ms
        .and_then(|ms| Timestamp::from_milliseconds_since_epoch(ms).ok());
    let reported_at = row
        .reported_at_ms
        .and_then(|ms| Timestamp::from_milliseconds_since_epoch(ms).ok());
    let outbox_reported_at = row
        .outbox_reported_at_ms
        .and_then(|ms| Timestamp::from_milliseconds_since_epoch(ms).ok());
    let lease_reported_at = row
        .lease_reported_at_ms
        .and_then(|ms| Timestamp::from_milliseconds_since_epoch(ms).ok());
    Ok(FleetRow {
        store_id: parse_registry_store(&row.store_id)
            .map_err(|error| FleetStoreError::new(error.to_string()))?,
        name: row.name,
        status: EntityStatus::from_db(&row.status),
        last_seen_at,
        last_config_pull_at,
        config_version_held: row.config_version_held,
        config_version_published: row.config_version_published,
        relay_backlog: u64::try_from(row.relay_backlog).unwrap_or(0),
        relay_oldest_pending_at,
        installed_version: row.installed_version,
        self_test_ok: row.self_test_ok,
        reported_at,
        // A negative depth cannot come from a store's log; if one somehow reached the column, "did
        // not say" is a truer answer for the console than a wrapped-around count.
        outbox_depth: row.outbox_depth.and_then(|depth| u64::try_from(depth).ok()),
        outbox_reported_at,
        // Same rule as the depth: a negative generation cannot have been written by this cloud, and
        // "did not say" is a truer answer for the console than a wrapped-around number that would
        // read as a store on an ancient lease.
        lease_generation_held: row
            .lease_generation_held
            .and_then(|value| u64::try_from(value).ok()),
        lease_reported_at,
        lease_generation_authoritative: row
            .lease_generation_authoritative
            .and_then(|value| u64::try_from(value).ok()),
    })
}

impl FleetStore for PostgresFleet {
    async fn list_fleet(&self, tenant: TenantId) -> Result<Vec<FleetRow>, FleetStoreError> {
        let rows = self
            .list(&tenant.to_string())
            .await
            .map_err(|error| FleetStoreError::new(error.to_string()))?;
        rows.into_iter().map(fleet_row).collect()
    }

    async fn store_detail(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> Result<Option<FleetRow>, FleetStoreError> {
        let row = self
            .fetch_one(&tenant.to_string(), &store.to_string())
            .await
            .map_err(|error| FleetStoreError::new(error.to_string()))?;
        row.map(fleet_row).transpose()
    }
}

// --- Background-task health (ADR-0068 slice 4) --------------------------------------------------

/// Converts one `store-postgres` task-health row into the cloud's [`TaskHealth`]. A tick instant out
/// of range (impossible for values this cloud wrote) or a detail that will not parse fails safe — the
/// instant to the epoch, the detail to an empty object — rather than dropping the whole listing, since
/// this is health telemetry and a decode fault must not itself read as "no health data".
fn task_health(row: TaskHealthRow) -> TaskHealth {
    let last_tick_at =
        Timestamp::from_milliseconds_since_epoch(row.last_tick_at_ms).unwrap_or(Timestamp::EPOCH);
    let detail = serde_json::from_str(&row.detail_json)
        .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
    TaskHealth {
        task: row.task,
        last_tick_at,
        detail,
    }
}

impl TaskHealthStore for PostgresTaskHealth {
    async fn record_tick(
        &self,
        task: &str,
        at: Timestamp,
        detail: &serde_json::Value,
    ) -> Result<(), TaskHealthError> {
        let detail_json = serde_json::to_string(detail).map_err(|error| {
            TaskHealthError::new(format!("encoding a task-health detail failed: {error}"))
        })?;
        self.record(task, at.as_milliseconds_since_epoch(), &detail_json)
            .await
            .map_err(|error| TaskHealthError::new(error.to_string()))
    }

    async fn list_health(&self) -> Result<Vec<TaskHealth>, TaskHealthError> {
        let rows = self
            .fetch_all()
            .await
            .map_err(|error| TaskHealthError::new(error.to_string()))?;
        Ok(rows.into_iter().map(task_health).collect())
    }
}

// --- Operational alerts (ADR-0073, Track O2) --------------------------------------------------

/// Converts one stored `alerts` row into the cloud's [`AlertRecord`]. A malformed id-tenant or an
/// unknown kind is store corruption (this cloud wrote well-formed values) and fails loudly; an
/// undecodable detail or an out-of-range instant fails safe (empty object / epoch) rather than
/// dropping the whole listing, since this is operational telemetry.
fn alert_record(row: AlertRow) -> Result<AlertRecord, AlertStoreError> {
    let tenant_id = match row.tenant_id {
        Some(text) => Some(
            text.parse::<Ulid>()
                .map(TenantId::new)
                .map_err(|_ignored| AlertStoreError::new("an alert tenant id is not a ULID"))?,
        ),
        None => None,
    };
    let kind = AlertKind::parse(&row.kind)
        .ok_or_else(|| AlertStoreError::new(format!("unknown alert kind: {}", row.kind)))?;
    let detail = serde_json::from_str(&row.detail_json)
        .unwrap_or_else(|_ignored| serde_json::Value::Object(serde_json::Map::new()));
    Ok(AlertRecord {
        id: row.id,
        tenant_id,
        kind,
        dedup_key: row.dedup_key,
        severity: AlertSeverity::parse(&row.severity),
        summary: row.summary,
        detail,
        first_seen_at: Timestamp::from_milliseconds_since_epoch(row.first_seen_at_ms)
            .unwrap_or(Timestamp::EPOCH),
        last_seen_at: Timestamp::from_milliseconds_since_epoch(row.last_seen_at_ms)
            .unwrap_or(Timestamp::EPOCH),
        resolved_at: row
            .resolved_at_ms
            .and_then(|ms| Timestamp::from_milliseconds_since_epoch(ms).ok()),
        acknowledged_at: row
            .acknowledged_at_ms
            .and_then(|ms| Timestamp::from_milliseconds_since_epoch(ms).ok()),
    })
}

impl AlertStore for PostgresAlerts {
    async fn upsert(&self, record: &AlertRecord) -> Result<(), AlertStoreError> {
        let tenant = record.tenant_id.map(|tenant| tenant.to_string());
        let detail_json = serde_json::to_string(&record.detail).map_err(|error| {
            AlertStoreError::new(format!("encoding an alert detail failed: {error}"))
        })?;
        self.upsert(
            &record.id,
            tenant.as_deref(),
            record.kind.as_str(),
            &record.dedup_key,
            record.severity.as_str(),
            &record.summary,
            &detail_json,
            record.first_seen_at.as_milliseconds_since_epoch(),
            record.last_seen_at.as_milliseconds_since_epoch(),
        )
        .await
        .map_err(|error| AlertStoreError::new(error.to_string()))
    }

    async fn resolve(&self, id: &str, resolved_at: Timestamp) -> Result<(), AlertStoreError> {
        self.resolve(id, resolved_at.as_milliseconds_since_epoch())
            .await
            .map_err(|error| AlertStoreError::new(error.to_string()))
    }

    async fn acknowledge(
        &self,
        id: &str,
        acknowledged_at: Timestamp,
    ) -> Result<(), AlertStoreError> {
        self.acknowledge(id, acknowledged_at.as_milliseconds_since_epoch())
            .await
            .map_err(|error| AlertStoreError::new(error.to_string()))
    }

    async fn list_active(&self) -> Result<Vec<AlertRecord>, AlertStoreError> {
        let rows = self
            .list_active()
            .await
            .map_err(|error| AlertStoreError::new(error.to_string()))?;
        rows.into_iter().map(alert_record).collect()
    }

    async fn list_recent(&self, limit: u32) -> Result<Vec<AlertRecord>, AlertStoreError> {
        let rows = self
            .list_recent(i64::from(limit))
            .await
            .map_err(|error| AlertStoreError::new(error.to_string()))?;
        rows.into_iter().map(alert_record).collect()
    }
}

// --- Tax rates (ADR-0074, Track M4) -----------------------------------------------------------

/// Converts one stored `catalog_tax_rates` row into a [`TaxRateEntry`], failing loudly on a malformed
/// class id (store corruption — this cloud wrote a well-formed ULID) but **skipping** an unrecognised
/// channel token: the seam speaks the closed `SalesChannel`, and a token from a future vocabulary is
/// dropped from the authoring view rather than failing the whole read (`None` from `filter_map`).
fn tax_rate_entry(row: &TaxRateRow) -> Result<Option<TaxRateEntry>, TaxRateStoreError> {
    let tax_class_id = row
        .tax_class_id
        .parse::<Ulid>()
        .map(TaxClassId::new)
        .map_err(|_ignored| {
            TaxRateStoreError::new(format!(
                "a tax-rate class id is not a ULID: {}",
                row.tax_class_id
            ))
        })?;
    let Some(sales_channel) = SalesChannel::from_wire(&row.sales_channel) else {
        return Ok(None);
    };
    let rate = TaxRate::from_basis_points(u32::try_from(row.rate_bps).unwrap_or(0));
    // A breakdown that does not decode is dropped rather than failing the read (ADR-0104): the
    // components are how the tax is *printed*, and `rate` — which does decode — is what the guest
    // pays, so a corrupt list can cost an invoice its lines and can never cost a bill its total.
    // The read still says so, because a silent drop on a legal document is not a thing to be quiet
    // about.
    let components: Vec<TaxComponent> = match serde_json::from_str(&row.components_json) {
        Ok(components) => components,
        Err(error) => {
            tracing::warn!(
                tax_class_id = %row.tax_class_id,
                sales_channel = %row.sales_channel,
                %error,
                "a stored tax-rate breakdown did not decode; the row keeps its rate and prints as                  one line"
            );
            Vec::new()
        }
    };
    Ok(Some(TaxRateEntry {
        tax_class_id,
        sales_channel,
        rate,
        components,
    }))
}

impl TaxRateStore for PostgresTaxRates {
    async fn list_tax_rates(
        &self,
        tenant_id: TenantId,
    ) -> Result<(Vec<TaxRateEntry>, Option<Version>), TaxRateStoreError> {
        let (rows, version) = self
            .fetch(&tenant_id.to_string())
            .await
            .map_err(|error| TaxRateStoreError::new(error.to_string()))?;
        let entries: Vec<TaxRateEntry> = rows
            .iter()
            .filter_map(|row| tax_rate_entry(row).transpose())
            .collect::<Result<_, _>>()?;
        Ok((entries, version.map(Version::new)))
    }

    async fn set_tax_rates(
        &self,
        tenant_id: TenantId,
        entries: &[TaxRateEntry],
        expected: Option<&Version>,
    ) -> Result<UpdateOutcome, TaxRateStoreError> {
        let rows: Vec<TaxRateRow> = entries
            .iter()
            .map(|entry| TaxRateRow {
                tax_class_id: entry.tax_class_id.to_string(),
                sales_channel: entry.sales_channel.as_wire().to_string(),
                rate_bps: i32::try_from(entry.rate.basis_points()).unwrap_or(i32::MAX),
                // A list of name/rate pairs cannot fail to serialise; `[]` on the impossible branch
                // rather than an `expect`, because the backbone forbids the panic and "no breakdown"
                // is the correct fallback anyway.
                components_json: serde_json::to_string(&entry.components)
                    .unwrap_or_else(|_ignored| "[]".to_owned()),
            })
            .collect();
        self.replace(&tenant_id.to_string(), &rows, expected.map(Version::as_str))
            .await
            .map(update_outcome)
            .map_err(|error| TaxRateStoreError::new(error.to_string()))
    }
}

impl CampaignStore for PostgresCampaigns {
    async fn list_campaigns(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<PublishedCampaign>>, CampaignStoreError> {
        let rows = self
            .fetch(&tenant_id.to_string())
            .await
            .map_err(|error| CampaignStoreError::new(error.to_string()))?;
        rows.iter().map(versioned_campaign).collect()
    }

    async fn get_campaign(
        &self,
        tenant_id: TenantId,
        campaign_id: CampaignId,
    ) -> Result<Option<Versioned<PublishedCampaign>>, CampaignStoreError> {
        let row = self
            .fetch_one(&tenant_id.to_string(), &campaign_id.to_string())
            .await
            .map_err(|error| CampaignStoreError::new(error.to_string()))?;
        row.as_ref().map(versioned_campaign).transpose()
    }

    async fn create_campaign(
        &self,
        tenant_id: TenantId,
        campaign: &PublishedCampaign,
    ) -> Result<CreateOutcome, CampaignStoreError> {
        let json = serde_json::to_string(campaign).map_err(|error| {
            CampaignStoreError::new(format!("could not serialize a campaign: {error}"))
        })?;
        self.insert(&tenant_id.to_string(), &campaign.id.to_string(), &json)
            .await
            .map(create_outcome)
            .map_err(|error| CampaignStoreError::new(error.to_string()))
    }

    async fn update_campaign(
        &self,
        tenant_id: TenantId,
        campaign: &PublishedCampaign,
        expected: &Version,
    ) -> Result<UpdateOutcome, CampaignStoreError> {
        let json = serde_json::to_string(campaign).map_err(|error| {
            CampaignStoreError::new(format!("could not serialize a campaign: {error}"))
        })?;
        self.update_at(
            &tenant_id.to_string(),
            &campaign.id.to_string(),
            &json,
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CampaignStoreError::new(error.to_string()))
    }

    async fn delete_campaign(
        &self,
        tenant_id: TenantId,
        campaign_id: CampaignId,
    ) -> Result<(), CampaignStoreError> {
        self.delete(&tenant_id.to_string(), &campaign_id.to_string())
            .await
            .map_err(|error| CampaignStoreError::new(error.to_string()))
    }
}

/// Decodes one stored campaign document, failing loudly on JSON this cloud wrote that no longer
/// parses — that is store corruption, not an absence.
fn decode_campaign(json: &str) -> Result<PublishedCampaign, CampaignStoreError> {
    serde_json::from_str(json).map_err(|error| {
        CampaignStoreError::new(format!("a stored campaign is not valid: {error}"))
    })
}

/// One stored campaign row as the seam returns it: the decoded campaign paired with the version the
/// read saw, which is the token [`CampaignStore::update_campaign`] demands back (ADR-0095).
fn versioned_campaign(
    row: &CampaignRow,
) -> Result<Versioned<PublishedCampaign>, CampaignStoreError> {
    Ok(Versioned::new(
        decode_campaign(&row.campaign_json)?,
        Version::new(row.version.clone()),
    ))
}

/// The `kind` discriminators the `inventory_items` table stores the three record kinds under.
const INVENTORY_KIND_INGREDIENT: &str = "ingredient";
const INVENTORY_KIND_RECIPE: &str = "recipe";
const INVENTORY_KIND_SUPPLIER: &str = "supplier";

/// Decodes one stored inventory document (a wire record), failing loudly on JSON this cloud wrote that
/// no longer parses — that is store corruption, not an absence.
fn decode_inventory<T: serde::de::DeserializeOwned>(json: &str) -> Result<T, InventoryStoreError> {
    serde_json::from_str(json).map_err(|error| {
        InventoryStoreError::new(format!("a stored inventory record is not valid: {error}"))
    })
}

/// One stored inventory row as the seam returns it: the decoded record paired with the version the
/// read saw, which is the token the matching `update_*` demands back (ADR-0095). Generic over the
/// three record kinds because the row shape is one table's.
fn versioned_inventory<T: serde::de::DeserializeOwned>(
    row: &InventoryRow,
) -> Result<Versioned<T>, InventoryStoreError> {
    Ok(Versioned::new(
        decode_inventory(&row.doc_json)?,
        Version::new(row.version.clone()),
    ))
}

impl InventoryStore for PostgresInventory {
    async fn list_ingredients(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<PublishedIngredient>>, InventoryStoreError> {
        let rows = self
            .fetch(&tenant_id.to_string(), INVENTORY_KIND_INGREDIENT)
            .await
            .map_err(|error| InventoryStoreError::new(error.to_string()))?;
        rows.iter().map(versioned_inventory).collect()
    }

    async fn get_ingredient(
        &self,
        tenant_id: TenantId,
        ingredient_id: IngredientId,
    ) -> Result<Option<Versioned<PublishedIngredient>>, InventoryStoreError> {
        let row = self
            .fetch_one(
                &tenant_id.to_string(),
                INVENTORY_KIND_INGREDIENT,
                &ingredient_id.to_string(),
            )
            .await
            .map_err(|error| InventoryStoreError::new(error.to_string()))?;
        row.as_ref().map(versioned_inventory).transpose()
    }

    async fn create_ingredient(
        &self,
        tenant_id: TenantId,
        ingredient: &PublishedIngredient,
    ) -> Result<CreateOutcome, InventoryStoreError> {
        let json = serde_json::to_string(ingredient).map_err(|error| {
            InventoryStoreError::new(format!("could not serialize an ingredient: {error}"))
        })?;
        self.insert(
            &tenant_id.to_string(),
            INVENTORY_KIND_INGREDIENT,
            &ingredient.id.to_string(),
            &json,
        )
        .await
        .map(create_outcome)
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }

    async fn update_ingredient(
        &self,
        tenant_id: TenantId,
        ingredient: &PublishedIngredient,
        expected: &Version,
    ) -> Result<UpdateOutcome, InventoryStoreError> {
        let json = serde_json::to_string(ingredient).map_err(|error| {
            InventoryStoreError::new(format!("could not serialize an ingredient: {error}"))
        })?;
        self.update_at(
            &tenant_id.to_string(),
            INVENTORY_KIND_INGREDIENT,
            &ingredient.id.to_string(),
            &json,
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }

    async fn delete_ingredient(
        &self,
        tenant_id: TenantId,
        ingredient_id: IngredientId,
    ) -> Result<(), InventoryStoreError> {
        self.delete(
            &tenant_id.to_string(),
            INVENTORY_KIND_INGREDIENT,
            &ingredient_id.to_string(),
        )
        .await
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }

    async fn list_recipes(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<PublishedRecipe>>, InventoryStoreError> {
        let rows = self
            .fetch(&tenant_id.to_string(), INVENTORY_KIND_RECIPE)
            .await
            .map_err(|error| InventoryStoreError::new(error.to_string()))?;
        rows.iter().map(versioned_inventory).collect()
    }

    async fn get_recipe(
        &self,
        tenant_id: TenantId,
        item: MenuItemId,
    ) -> Result<Option<Versioned<PublishedRecipe>>, InventoryStoreError> {
        let row = self
            .fetch_one(
                &tenant_id.to_string(),
                INVENTORY_KIND_RECIPE,
                &item.to_string(),
            )
            .await
            .map_err(|error| InventoryStoreError::new(error.to_string()))?;
        row.as_ref().map(versioned_inventory).transpose()
    }

    async fn create_recipe(
        &self,
        tenant_id: TenantId,
        recipe: &PublishedRecipe,
    ) -> Result<CreateOutcome, InventoryStoreError> {
        let json = serde_json::to_string(recipe).map_err(|error| {
            InventoryStoreError::new(format!("could not serialize a recipe: {error}"))
        })?;
        self.insert(
            &tenant_id.to_string(),
            INVENTORY_KIND_RECIPE,
            &recipe.item.to_string(),
            &json,
        )
        .await
        .map(create_outcome)
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }

    async fn update_recipe(
        &self,
        tenant_id: TenantId,
        recipe: &PublishedRecipe,
        expected: &Version,
    ) -> Result<UpdateOutcome, InventoryStoreError> {
        let json = serde_json::to_string(recipe).map_err(|error| {
            InventoryStoreError::new(format!("could not serialize a recipe: {error}"))
        })?;
        self.update_at(
            &tenant_id.to_string(),
            INVENTORY_KIND_RECIPE,
            &recipe.item.to_string(),
            &json,
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }

    async fn delete_recipe(
        &self,
        tenant_id: TenantId,
        item: MenuItemId,
    ) -> Result<(), InventoryStoreError> {
        self.delete(
            &tenant_id.to_string(),
            INVENTORY_KIND_RECIPE,
            &item.to_string(),
        )
        .await
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }

    async fn list_suppliers(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<PublishedSupplier>>, InventoryStoreError> {
        let rows = self
            .fetch(&tenant_id.to_string(), INVENTORY_KIND_SUPPLIER)
            .await
            .map_err(|error| InventoryStoreError::new(error.to_string()))?;
        rows.iter().map(versioned_inventory).collect()
    }

    async fn get_supplier(
        &self,
        tenant_id: TenantId,
        supplier_id: SupplierId,
    ) -> Result<Option<Versioned<PublishedSupplier>>, InventoryStoreError> {
        let row = self
            .fetch_one(
                &tenant_id.to_string(),
                INVENTORY_KIND_SUPPLIER,
                &supplier_id.to_string(),
            )
            .await
            .map_err(|error| InventoryStoreError::new(error.to_string()))?;
        row.as_ref().map(versioned_inventory).transpose()
    }

    async fn create_supplier(
        &self,
        tenant_id: TenantId,
        supplier: &PublishedSupplier,
    ) -> Result<CreateOutcome, InventoryStoreError> {
        let json = serde_json::to_string(supplier).map_err(|error| {
            InventoryStoreError::new(format!("could not serialize a supplier: {error}"))
        })?;
        self.insert(
            &tenant_id.to_string(),
            INVENTORY_KIND_SUPPLIER,
            &supplier.id.to_string(),
            &json,
        )
        .await
        .map(create_outcome)
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }

    async fn update_supplier(
        &self,
        tenant_id: TenantId,
        supplier: &PublishedSupplier,
        expected: &Version,
    ) -> Result<UpdateOutcome, InventoryStoreError> {
        let json = serde_json::to_string(supplier).map_err(|error| {
            InventoryStoreError::new(format!("could not serialize a supplier: {error}"))
        })?;
        self.update_at(
            &tenant_id.to_string(),
            INVENTORY_KIND_SUPPLIER,
            &supplier.id.to_string(),
            &json,
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }

    async fn delete_supplier(
        &self,
        tenant_id: TenantId,
        supplier_id: SupplierId,
    ) -> Result<(), InventoryStoreError> {
        self.delete(
            &tenant_id.to_string(),
            INVENTORY_KIND_SUPPLIER,
            &supplier_id.to_string(),
        )
        .await
        .map_err(|error| InventoryStoreError::new(error.to_string()))
    }
}

impl VoucherStore for PostgresVouchers {
    async fn insert_batch(
        &self,
        tenant_id: TenantId,
        vouchers: &[NewVoucher],
    ) -> Result<(), VoucherStoreError> {
        // Own the id/code strings for the duration of the call; the adapter's row type borrows them.
        let owned: Vec<(String, String, String)> = vouchers
            .iter()
            .map(|voucher| {
                (
                    voucher.voucher_id.to_string(),
                    voucher.campaign_id.to_string(),
                    voucher.code.clone(),
                )
            })
            .collect();
        let rows: Vec<NewVoucherRow<'_>> = owned
            .iter()
            .map(|(voucher_id, campaign_id, code)| NewVoucherRow {
                voucher_id,
                campaign_id,
                code,
            })
            .collect();
        self.insert_batch(&tenant_id.to_string(), &rows)
            .await
            .map_err(|error| VoucherStoreError::new(error.to_string()))
    }

    async fn list_by_campaign(
        &self,
        tenant_id: TenantId,
        campaign_id: CampaignId,
    ) -> Result<Vec<VoucherRecord>, VoucherStoreError> {
        let rows = self
            .list_by_campaign(&tenant_id.to_string(), &campaign_id.to_string())
            .await
            .map_err(|error| VoucherStoreError::new(error.to_string()))?;
        Ok(rows.into_iter().map(voucher_record).collect())
    }

    async fn list_by_campaign_page(
        &self,
        tenant_id: TenantId,
        campaign_id: CampaignId,
        page: PageRequest,
    ) -> Result<Page<VoucherRecord>, VoucherStoreError> {
        // `PageRequest` guarantees the values are in range (`1..=MAX_PAGE_LIMIT`, offset within
        // `MAX_PAGE_OFFSET`), so widening `u32` into the `i64` the SQL binds cannot lose or
        // sign-flip anything. That guarantee is why the adapter takes bare integers and does not
        // re-check them.
        let (rows, total) = self
            .list_by_campaign_page(
                &tenant_id.to_string(),
                &campaign_id.to_string(),
                i64::from(page.limit()),
                i64::from(page.offset()),
            )
            .await
            .map_err(|error| VoucherStoreError::new(error.to_string()))?;
        Ok(Page::new(
            rows.into_iter().map(voucher_record).collect(),
            u32::try_from(total).unwrap_or(u32::MAX),
        ))
    }
}

/// Rehydrates one stored voucher row into the seam's record.
///
/// Shared by the paged and unpaged reads: a `status` token that decoded differently on one of them
/// would show an operator a live code as void on page two and active on the flyer run.
fn voucher_record(row: VoucherRow) -> VoucherRecord {
    VoucherRecord {
        voucher_id: row.voucher_id,
        campaign_id: row.campaign_id,
        code: row.code,
        status: VoucherStatus::from_wire(&row.status),
        created_at_ms: row.created_at_ms,
    }
}

/// Rehydrates a stored scheduled-publish row into the seam's domain type, failing loudly on an id or
/// node this cloud wrote that no longer parses — store corruption, not an absence.
fn scheduled_from_row(row: ScheduledPublishRow) -> Result<ScheduledPublish, ScheduledPublishError> {
    let tenant_id = row
        .tenant_id
        .parse::<Ulid>()
        .map(TenantId::new)
        .map_err(|_ignored| {
            ScheduledPublishError::new("a scheduled publish has a non-ULID tenant")
        })?;
    let store_id = row
        .store_id
        .parse::<Ulid>()
        .map(StoreId::new)
        .map_err(|_ignored| {
            ScheduledPublishError::new("a scheduled publish has a non-ULID store")
        })?;
    let node_value = serde_json::from_str(&row.node_value_json).map_err(|error| {
        ScheduledPublishError::new(format!("a scheduled node is not valid JSON: {error}"))
    })?;
    Ok(ScheduledPublish {
        id: row.id,
        tenant_id,
        store_id,
        node_key: row.node_key,
        node_value,
        effective_at_ms: row.effective_at_ms,
        status: ScheduledPublishStatus::from_wire(&row.status),
        created_at_ms: row.created_at_ms,
        applied_version_id: row.applied_version_id,
    })
}

impl ScheduledPublishStore for PostgresScheduledPublishes {
    async fn schedule(&self, publish: &NewScheduledPublish) -> Result<(), ScheduledPublishError> {
        let node_value_json = serde_json::to_string(&publish.node_value).map_err(|error| {
            ScheduledPublishError::new(format!("could not serialize a scheduled node: {error}"))
        })?;
        let tenant = publish.tenant_id.to_string();
        let store = publish.store_id.to_string();
        let row = NewScheduledPublishRow {
            id: &publish.id,
            tenant_id: &tenant,
            store_id: &store,
            node_key: &publish.node_key,
            node_value_json: &node_value_json,
            effective_at_ms: publish.effective_at_ms,
            created_by: &publish.created_by,
        };
        self.schedule(&row)
            .await
            .map_err(|error| ScheduledPublishError::new(error.to_string()))
    }

    async fn due(&self, now_ms: i64) -> Result<Vec<ScheduledPublish>, ScheduledPublishError> {
        let rows = self
            .due(now_ms)
            .await
            .map_err(|error| ScheduledPublishError::new(error.to_string()))?;
        rows.into_iter().map(scheduled_from_row).collect()
    }

    async fn list_for_store(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<ScheduledPublish>, ScheduledPublishError> {
        let rows = self
            .list_for_store(&tenant_id.to_string(), &store_id.to_string())
            .await
            .map_err(|error| ScheduledPublishError::new(error.to_string()))?;
        rows.into_iter().map(scheduled_from_row).collect()
    }

    async fn cancel(&self, tenant_id: TenantId, id: &str) -> Result<bool, ScheduledPublishError> {
        self.cancel(&tenant_id.to_string(), id)
            .await
            .map_err(|error| ScheduledPublishError::new(error.to_string()))
    }

    async fn mark_applied(&self, id: &str, version_id: &str) -> Result<(), ScheduledPublishError> {
        self.mark_applied(id, version_id)
            .await
            .map_err(|error| ScheduledPublishError::new(error.to_string()))
    }
}

// --- Media renditions (ADR-0075) --------------------------------------------------------------

/// Converts one stored `media_assets` summary row into the seam's [`MediaSummary`], failing loudly on
/// a media id that is not a ULID — that is store corruption (this cloud minted it), not an absence.
fn media_summary(row: MediaAssetRow) -> Result<MediaSummary, MediaStoreError> {
    let media_id = row
        .media_id
        .parse::<Ulid>()
        .map(MediaId::new)
        .map_err(|_ignored| {
            MediaStoreError::new(format!("a media id is not a ULID: {}", row.media_id))
        })?;
    Ok(MediaSummary {
        media_id,
        content_type: row.content_type,
        detail_bytes: usize::try_from(row.detail_bytes).unwrap_or(0),
        created_at_ms: row.created_at_ms,
    })
}

impl MediaStore for PostgresMedia {
    async fn put(&self, asset: &NewMediaAsset) -> Result<(), MediaStoreError> {
        let detail_bytes = i32::try_from(asset.detail.len()).unwrap_or(i32::MAX);
        self.insert(
            &asset.media_id.to_string(),
            &asset.tenant_id.to_string(),
            &asset.content_type,
            &asset.thumbnail,
            &asset.detail,
            detail_bytes,
        )
        .await
        .map_err(|error| MediaStoreError::new(error.to_string()))
    }

    async fn get(
        &self,
        tenant_id: TenantId,
        media_id: MediaId,
        rendition: Rendition,
    ) -> Result<Option<Vec<u8>>, MediaStoreError> {
        self.fetch_rendition(
            &tenant_id.to_string(),
            &media_id.to_string(),
            matches!(rendition, Rendition::Detail),
        )
        .await
        .map_err(|error| MediaStoreError::new(error.to_string()))
    }

    async fn list(&self, tenant_id: TenantId) -> Result<Vec<MediaSummary>, MediaStoreError> {
        let rows = self
            .fetch_summaries(&tenant_id.to_string())
            .await
            .map_err(|error| MediaStoreError::new(error.to_string()))?;
        rows.into_iter().map(media_summary).collect()
    }

    async fn list_page(
        &self,
        tenant_id: TenantId,
        page: PageRequest,
    ) -> Result<Page<MediaSummary>, MediaStoreError> {
        // `PageRequest` already range-checked these, so widening `u32` into the `i64` the SQL binds
        // cannot lose or sign-flip anything — which is why the adapter takes bare integers.
        let (rows, total) = self
            .fetch_summaries_page(
                &tenant_id.to_string(),
                i64::from(page.limit()),
                i64::from(page.offset()),
            )
            .await
            .map_err(|error| MediaStoreError::new(error.to_string()))?;
        let items = rows
            .into_iter()
            .map(media_summary)
            .collect::<Result<Vec<MediaSummary>, MediaStoreError>>()?;
        Ok(Page::new(items, u32::try_from(total).unwrap_or(u32::MAX)))
    }

    async fn delete(
        &self,
        tenant_id: TenantId,
        media_id: MediaId,
    ) -> Result<bool, MediaStoreError> {
        self.remove(&tenant_id.to_string(), &media_id.to_string())
            .await
            .map_err(|error| MediaStoreError::new(error.to_string()))
    }
}

// --- The console audit trail (ADR-0069) -------------------------------------------------------

/// Converts one stored `audit_log` row into the cloud's [`AuditEntry`], failing loudly on a
/// malformed id/tenant/role/timestamp or an undecodable before/after — every one of those is store
/// corruption (this cloud wrote well-formed values), not an ordinary absence.
fn audit_entry(row: AuditLogRow) -> Result<AuditEntry, AuditStoreError> {
    let id = row
        .id
        .parse::<Ulid>()
        .map(AuditId::new)
        .map_err(|_ignored| {
            AuditStoreError::new(format!("an audit id is not a ULID: {}", row.id))
        })?;
    let tenant_id = match row.tenant_id {
        Some(text) => Some(
            text.parse::<Ulid>()
                .map(TenantId::new)
                .map_err(|_ignored| AuditStoreError::new("an audit tenant id is not a ULID"))?,
        ),
        None => None,
    };
    let role = AdminRole::from_token(&row.actor_role).ok_or_else(|| {
        AuditStoreError::new(format!(
            "unknown audit actor role token: {}",
            row.actor_role
        ))
    })?;
    let before = decode_audit_value(row.before_json, "before")?;
    let after = decode_audit_value(row.after_json, "after")?;
    let at = Timestamp::from_milliseconds_since_epoch(row.at_ms)
        .map_err(|_ignored| AuditStoreError::new("an audit timestamp is out of range"))?;
    Ok(AuditEntry {
        id,
        tenant_id,
        actor: AuditActor {
            admin_id: row.actor_admin_id,
            email: row.actor_email,
            role,
        },
        action: row.action,
        entity_type: row.entity_type,
        entity_id: row.entity_id,
        before,
        after,
        request_id: row.request_id,
        at,
    })
}

/// Decodes a stored audit before/after JSON document, or `None` if the column was `NULL`.
fn decode_audit_value(
    text: Option<String>,
    which: &str,
) -> Result<Option<serde_json::Value>, AuditStoreError> {
    match text {
        Some(json) => serde_json::from_str(&json).map(Some).map_err(|error| {
            AuditStoreError::new(format!("decoding an audit {which} value failed: {error}"))
        }),
        None => Ok(None),
    }
}

/// Encodes an audit before/after value to JSON text for storage, or `None` if absent.
fn encode_audit_value(
    value: Option<&serde_json::Value>,
    which: &str,
) -> Result<Option<String>, AuditStoreError> {
    match value {
        Some(value) => serde_json::to_string(value).map(Some).map_err(|error| {
            AuditStoreError::new(format!("encoding an audit {which} value failed: {error}"))
        }),
        None => Ok(None),
    }
}

impl AuditStore for PostgresAudit {
    async fn append(&self, entry: &AuditEntry) -> Result<(), AuditStoreError> {
        let tenant = entry.tenant_id.map(|tenant| tenant.to_string());
        let before = encode_audit_value(entry.before.as_ref(), "before")?;
        let after = encode_audit_value(entry.after.as_ref(), "after")?;
        self.insert(
            &entry.id.to_string(),
            tenant.as_deref(),
            &entry.actor.admin_id,
            &entry.actor.email,
            entry.actor.role.as_token(),
            &entry.action,
            &entry.entity_type,
            &entry.entity_id,
            before.as_deref(),
            after.as_deref(),
            entry.request_id.as_deref(),
            entry.at.as_milliseconds_since_epoch(),
        )
        .await
        .map_err(|error| AuditStoreError::new(error.to_string()))
    }

    async fn list(
        &self,
        tenant: Option<TenantId>,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, AuditStoreError> {
        let tenant = tenant.map(|tenant| tenant.to_string());
        let rows = self
            .fetch(tenant.as_deref(), i64::from(limit))
            .await
            .map_err(|error| AuditStoreError::new(error.to_string()))?;
        rows.into_iter().map(audit_entry).collect()
    }

    async fn query(
        &self,
        filter: &AuditQuery,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, AuditStoreError> {
        let tenant = filter.tenant.map(|tenant| tenant.to_string());
        let rows = self
            .search(
                tenant.as_deref(),
                filter.entity_type.as_deref(),
                filter.entity_id.as_deref(),
                filter.action.as_deref(),
                filter.actor_admin_id.as_deref(),
                filter.since_ms,
                filter.until_ms,
                i64::from(limit),
            )
            .await
            .map_err(|error| AuditStoreError::new(error.to_string()))?;
        rows.into_iter().map(audit_entry).collect()
    }

    async fn query_page(
        &self,
        filter: &AuditQuery,
        page: PageRequest,
        order: TrailOrder,
    ) -> Result<Page<AuditEntry>, AuditStoreError> {
        let tenant = filter.tenant.map(|tenant| tenant.to_string());
        // The one place the trail's order vocabulary meets the adapter's. Exhaustive by necessity:
        // `clippy::wildcard_enum_match_arm` is denied, so a new `TrailOrder` variant fails to
        // compile here rather than quietly falling back to newest-first.
        let order = match order {
            TrailOrder::Newest => AuditOrder::Newest,
            TrailOrder::Oldest => AuditOrder::Oldest,
        };
        let (rows, total) = self
            .search_page(
                tenant.as_deref(),
                filter.entity_type.as_deref(),
                filter.entity_id.as_deref(),
                filter.action.as_deref(),
                filter.actor_admin_id.as_deref(),
                filter.since_ms,
                filter.until_ms,
                order,
                i64::from(page.limit()),
                i64::from(page.offset()),
            )
            .await
            .map_err(|error| AuditStoreError::new(error.to_string()))?;
        let entries: Vec<AuditEntry> = rows
            .into_iter()
            .map(audit_entry)
            .collect::<Result<_, _>>()?;
        Ok(Page::new(entries, u32::try_from(total).unwrap_or(u32::MAX)))
    }
}

// --- people & access (Track M1, ADR-0070): the `employees` rows converted to the EmployeeStore domain ---

/// Converts a stored employee row into the domain [`Employee`], parsing the ids and status. A row with
/// an unparseable id is corruption the caller should see, not silently drop.
fn employee_record(row: EmployeeRow) -> Result<Versioned<Employee>, EmployeeStoreError> {
    let employee_id = row
        .id
        .parse::<Ulid>()
        .map(EmployeeId::new)
        .map_err(|error| {
            EmployeeStoreError::new(format!("stored employee id is not a ULID: {error}"))
        })?;
    let tenant_id = row
        .tenant_id
        .parse::<Ulid>()
        .map(TenantId::new)
        .map_err(|error| {
            EmployeeStoreError::new(format!("stored tenant id is not a ULID: {error}"))
        })?;
    let version = Version::new(row.version);
    Ok(Versioned {
        record: Employee {
            employee_id,
            tenant_id,
            code: row.code,
            name: row.name,
            status: EntityStatus::from_db(&row.status),
            has_pin: row.has_pin,
        },
        etag: version,
    })
}

impl EmployeeStore for PostgresPeople {
    async fn create(&self, employee: &NewEmployee) -> Result<Version, EmployeeStoreError> {
        self.insert(
            &employee.employee_id.to_string(),
            &employee.tenant_id.to_string(),
            &employee.code,
            &employee.name,
        )
        .await
        .map(Version::new)
        .map_err(|error| EmployeeStoreError::new(error.to_string()))
    }

    async fn list(&self, tenant: TenantId) -> Result<Vec<Versioned<Employee>>, EmployeeStoreError> {
        let rows = self
            .fetch(&tenant.to_string())
            .await
            .map_err(|error| EmployeeStoreError::new(error.to_string()))?;
        rows.into_iter().map(employee_record).collect()
    }

    async fn list_page(
        &self,
        tenant: TenantId,
        page: PageRequest,
        filter: &EmployeeListFilter,
    ) -> Result<Page<Versioned<Employee>>, EmployeeStoreError> {
        // The one place the wire's sort token becomes an `ORDER BY`. `wildcard_enum_match_arm` is
        // denied workspace-wide, so a new `EmployeeSort` variant fails here rather than quietly
        // ordering by `created_at`.
        let order = match filter.sort {
            EmployeeSort::Newest => EmployeeOrder::Newest,
            EmployeeSort::Name => EmployeeOrder::Name,
            EmployeeSort::Code => EmployeeOrder::Code,
        };
        let (rows, total) = self
            .fetch_page(
                &tenant.to_string(),
                filter.search.as_deref(),
                order,
                filter.descending,
                i64::from(page.limit()),
                i64::from(page.offset()),
            )
            .await
            .map_err(|error| EmployeeStoreError::new(error.to_string()))?;
        let employees: Vec<Versioned<Employee>> = rows
            .into_iter()
            .map(employee_record)
            .collect::<Result<_, _>>()?;
        Ok(Page::new(
            employees,
            u32::try_from(total).unwrap_or(u32::MAX),
        ))
    }

    async fn get(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> Result<Option<Versioned<Employee>>, EmployeeStoreError> {
        let row = self
            .fetch_one(&tenant.to_string(), &employee_id.to_string())
            .await
            .map_err(|error| EmployeeStoreError::new(error.to_string()))?;
        row.map(employee_record).transpose()
    }

    async fn update(
        &self,
        employee: &EmployeeUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, EmployeeStoreError> {
        self.set(
            &employee.tenant_id.to_string(),
            &employee.employee_id.to_string(),
            &employee.name,
            employee.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| EmployeeStoreError::new(error.to_string()))
    }

    async fn set_pin(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
        pin_phc: &str,
    ) -> Result<bool, EmployeeStoreError> {
        PostgresPeople::set_pin(self, &tenant.to_string(), &employee_id.to_string(), pin_phc)
            .await
            .map_err(|error| EmployeeStoreError::new(error.to_string()))
    }

    async fn pin_phc(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> Result<Option<String>, EmployeeStoreError> {
        PostgresPeople::pin_phc(self, &tenant.to_string(), &employee_id.to_string())
            .await
            .map_err(|error| EmployeeStoreError::new(error.to_string()))
    }
}

/// Converts a stored role-template row into the domain [`RoleTemplate`], parsing the ids, status, and
/// the `jsonb` permission array.
fn role_template_record(
    row: RoleTemplateRow,
) -> Result<Versioned<RoleTemplate>, RoleTemplateStoreError> {
    let role_template_id = row
        .id
        .parse::<Ulid>()
        .map(RoleTemplateId::new)
        .map_err(|error| {
            RoleTemplateStoreError::new(format!("stored role-template id is not a ULID: {error}"))
        })?;
    let tenant_id = row
        .tenant_id
        .parse::<Ulid>()
        .map(TenantId::new)
        .map_err(|error| {
            RoleTemplateStoreError::new(format!("stored tenant id is not a ULID: {error}"))
        })?;
    let permissions: Vec<String> =
        serde_json::from_str(&row.permissions_json).map_err(|error| {
            RoleTemplateStoreError::new(format!(
                "stored role-template permissions are not JSON: {error}"
            ))
        })?;
    let version = Version::new(row.version);
    Ok(Versioned {
        record: RoleTemplate {
            role_template_id,
            tenant_id,
            name: row.name,
            permissions,
            status: EntityStatus::from_db(&row.status),
        },
        etag: version,
    })
}

impl RoleTemplateStore for PostgresPeople {
    async fn create(&self, template: &NewRoleTemplate) -> Result<Version, RoleTemplateStoreError> {
        let permissions_json = serde_json::to_string(&template.permissions).map_err(|error| {
            RoleTemplateStoreError::new(format!("cannot serialize permissions: {error}"))
        })?;
        self.insert_role_template(
            &template.role_template_id.to_string(),
            &template.tenant_id.to_string(),
            &template.name,
            &permissions_json,
        )
        .await
        .map(Version::new)
        .map_err(|error| RoleTemplateStoreError::new(error.to_string()))
    }

    async fn list(
        &self,
        tenant: TenantId,
    ) -> Result<Vec<Versioned<RoleTemplate>>, RoleTemplateStoreError> {
        let rows = self
            .fetch_role_templates(&tenant.to_string())
            .await
            .map_err(|error| RoleTemplateStoreError::new(error.to_string()))?;
        rows.into_iter().map(role_template_record).collect()
    }

    async fn get(
        &self,
        tenant: TenantId,
        role_template_id: RoleTemplateId,
    ) -> Result<Option<Versioned<RoleTemplate>>, RoleTemplateStoreError> {
        let row = self
            .fetch_role_template(&tenant.to_string(), &role_template_id.to_string())
            .await
            .map_err(|error| RoleTemplateStoreError::new(error.to_string()))?;
        row.map(role_template_record).transpose()
    }

    async fn update(
        &self,
        template: &RoleTemplateUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, RoleTemplateStoreError> {
        let permissions_json = serde_json::to_string(&template.permissions).map_err(|error| {
            RoleTemplateStoreError::new(format!("cannot serialize permissions: {error}"))
        })?;
        self.set_role_template(
            &template.tenant_id.to_string(),
            &template.role_template_id.to_string(),
            &template.name,
            &permissions_json,
            template.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| RoleTemplateStoreError::new(error.to_string()))
    }
}

/// Converts a stored assignment row into the domain [`Assignment`], parsing the four ids.
fn assignment_record(row: &AssignmentRow) -> Result<Assignment, AssignmentStoreError> {
    let assignment_id = row
        .id
        .parse::<Ulid>()
        .map(AssignmentId::new)
        .map_err(|error| {
            AssignmentStoreError::new(format!("stored assignment id is not a ULID: {error}"))
        })?;
    let tenant_id = row
        .tenant_id
        .parse::<Ulid>()
        .map(TenantId::new)
        .map_err(|error| {
            AssignmentStoreError::new(format!("stored tenant id is not a ULID: {error}"))
        })?;
    let employee_id = row
        .employee_id
        .parse::<Ulid>()
        .map(EmployeeId::new)
        .map_err(|error| {
            AssignmentStoreError::new(format!("stored employee id is not a ULID: {error}"))
        })?;
    let store_id = row
        .store_id
        .parse::<Ulid>()
        .map(StoreId::new)
        .map_err(|error| {
            AssignmentStoreError::new(format!("stored store id is not a ULID: {error}"))
        })?;
    let role_template_id = row
        .role_template_id
        .parse::<Ulid>()
        .map(RoleTemplateId::new)
        .map_err(|error| {
            AssignmentStoreError::new(format!("stored role-template id is not a ULID: {error}"))
        })?;
    Ok(Assignment {
        assignment_id,
        tenant_id,
        employee_id,
        store_id,
        role_template_id,
        employee_name: row.employee_name.clone(),
        employee_code: row.employee_code.clone(),
    })
}

impl AssignmentStore for PostgresPeople {
    async fn assign(&self, assignment: &NewAssignment) -> Result<(), AssignmentStoreError> {
        self.insert_assignment(
            &assignment.assignment_id.to_string(),
            &assignment.tenant_id.to_string(),
            &assignment.employee_id.to_string(),
            &assignment.store_id.to_string(),
            &assignment.role_template_id.to_string(),
        )
        .await
        .map_err(|error| AssignmentStoreError::new(error.to_string()))
    }

    async fn list_for_store(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        let rows = self
            .fetch_assignments_for_store(&tenant.to_string(), &store_id.to_string())
            .await
            .map_err(|error| AssignmentStoreError::new(error.to_string()))?;
        rows.iter().map(assignment_record).collect()
    }

    async fn list_for_employee(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        let rows = self
            .fetch_assignments_for_employee(&tenant.to_string(), &employee_id.to_string())
            .await
            .map_err(|error| AssignmentStoreError::new(error.to_string()))?;
        rows.iter().map(assignment_record).collect()
    }

    async fn remove(
        &self,
        tenant: TenantId,
        assignment_id: AssignmentId,
    ) -> Result<bool, AssignmentStoreError> {
        self.delete_assignment(&tenant.to_string(), &assignment_id.to_string())
            .await
            .map_err(|error| AssignmentStoreError::new(error.to_string()))
    }
}

// --- floor (Track M2, ADR-0072): store-postgres rows converted to the AreaStore/TableStore domain ---

/// Parses a stored ULID string, naming the field in the error for the server log.
fn parse_floor_ulid(text: &str, what: &str) -> Result<Ulid, FloorStoreError> {
    text.parse::<Ulid>()
        .map_err(|error| FloorStoreError::new(format!("stored {what} id is not a ULID: {error}")))
}

/// Reads one queried row into an [`Area`].
fn area_record(row: AreaRow) -> Result<Versioned<Area>, FloorStoreError> {
    let record = Area {
        area_id: AreaId::new(parse_floor_ulid(&row.id, "area")?),
        tenant_id: TenantId::new(parse_floor_ulid(&row.tenant_id, "tenant")?),
        store_id: StoreId::new(parse_floor_ulid(&row.store_id, "store")?),
        name: row.name,
        status: EntityStatus::from_db(&row.status),
    };
    Ok(Versioned {
        record,
        etag: Version::new(row.version),
    })
}

/// Reads one queried row into a [`Table`], folding the two nullable grid columns into an optional
/// [`GridPosition`] (a table is placed only when both are set).
fn table_record(row: TableRow) -> Result<Versioned<Table>, FloorStoreError> {
    let position = match (row.grid_column, row.grid_row) {
        (Some(column), Some(grid_row)) => Some(GridPosition {
            column: u16::try_from(column).unwrap_or(0),
            row: u16::try_from(grid_row).unwrap_or(0),
        }),
        _ => None,
    };
    let record = Table {
        table_id: TableId::new(parse_floor_ulid(&row.id, "table")?),
        tenant_id: TenantId::new(parse_floor_ulid(&row.tenant_id, "tenant")?),
        store_id: StoreId::new(parse_floor_ulid(&row.store_id, "store")?),
        area_id: AreaId::new(parse_floor_ulid(&row.area_id, "area")?),
        label: row.label,
        seats: u16::try_from(row.seats).unwrap_or(0),
        position,
        status: EntityStatus::from_db(&row.status),
    };
    Ok(Versioned {
        record,
        etag: Version::new(row.version),
    })
}

impl AreaStore for PostgresFloor {
    async fn create(&self, area: &NewArea) -> Result<Version, FloorStoreError> {
        self.insert_area(
            &area.area_id.to_string(),
            &area.tenant_id.to_string(),
            &area.store_id.to_string(),
            &area.name,
        )
        .await
        .map(Version::new)
        .map_err(|error| FloorStoreError::new(error.to_string()))
    }

    async fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Versioned<Area>>, FloorStoreError> {
        let rows = self
            .fetch_areas(&tenant.to_string(), &store_id.to_string())
            .await
            .map_err(|error| FloorStoreError::new(error.to_string()))?;
        rows.into_iter().map(area_record).collect()
    }

    async fn get(
        &self,
        tenant: TenantId,
        area_id: AreaId,
    ) -> Result<Option<Versioned<Area>>, FloorStoreError> {
        let row = self
            .fetch_area(&tenant.to_string(), &area_id.to_string())
            .await
            .map_err(|error| FloorStoreError::new(error.to_string()))?;
        row.map(area_record).transpose()
    }

    async fn update(
        &self,
        area: &AreaUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, FloorStoreError> {
        self.set_area(
            &area.tenant_id.to_string(),
            &area.area_id.to_string(),
            &area.name,
            area.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| FloorStoreError::new(error.to_string()))
    }
}

impl TableStore for PostgresFloor {
    async fn create(&self, table: &NewTable) -> Result<Version, FloorStoreError> {
        self.insert_table(
            &table.table_id.to_string(),
            &table.tenant_id.to_string(),
            &table.store_id.to_string(),
            &table.area_id.to_string(),
            &table.label,
            i32::from(table.seats),
            table.position.map(|position| i32::from(position.column)),
            table.position.map(|position| i32::from(position.row)),
        )
        .await
        .map(Version::new)
        .map_err(|error| FloorStoreError::new(error.to_string()))
    }

    async fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Versioned<Table>>, FloorStoreError> {
        let rows = self
            .fetch_tables(&tenant.to_string(), &store_id.to_string())
            .await
            .map_err(|error| FloorStoreError::new(error.to_string()))?;
        rows.into_iter().map(table_record).collect()
    }

    async fn get(
        &self,
        tenant: TenantId,
        table_id: TableId,
    ) -> Result<Option<Versioned<Table>>, FloorStoreError> {
        let row = self
            .fetch_table(&tenant.to_string(), &table_id.to_string())
            .await
            .map_err(|error| FloorStoreError::new(error.to_string()))?;
        row.map(table_record).transpose()
    }

    async fn update(
        &self,
        table: &TableUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, FloorStoreError> {
        self.set_table(
            &table.tenant_id.to_string(),
            &table.table_id.to_string(),
            &table.area_id.to_string(),
            &table.label,
            i32::from(table.seats),
            table.position.map(|position| i32::from(position.column)),
            table.position.map(|position| i32::from(position.row)),
            table.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| FloorStoreError::new(error.to_string()))
    }
}

/// Reads one queried row into a [`Station`].
fn station_record(row: StationRow) -> Result<Versioned<Station>, FloorStoreError> {
    let backup_station_id = row
        .backup_station_id
        .as_deref()
        .map(|text| parse_floor_ulid(text, "backup station").map(StationId::new))
        .transpose()?;
    let record = Station {
        station_id: StationId::new(parse_floor_ulid(&row.id, "station")?),
        tenant_id: TenantId::new(parse_floor_ulid(&row.tenant_id, "tenant")?),
        store_id: StoreId::new(parse_floor_ulid(&row.store_id, "store")?),
        name: row.name,
        backup_station_id,
        is_default: row.is_default,
        status: EntityStatus::from_db(&row.status),
    };
    Ok(Versioned {
        record,
        etag: Version::new(row.version),
    })
}

/// Reads one queried row into a [`RoutingRule`].
fn routing_rule_record(row: &RoutingRuleRow) -> Result<RoutingRule, FloorStoreError> {
    let menu_item_id = row
        .menu_item_id
        .as_deref()
        .map(|text| parse_floor_ulid(text, "menu item").map(MenuItemId::new))
        .transpose()?;
    let course_id = row
        .course_id
        .as_deref()
        .map(|text| parse_floor_ulid(text, "course").map(CourseId::new))
        .transpose()?;
    Ok(RoutingRule {
        rule_id: RoutingRuleId::new(parse_floor_ulid(&row.id, "routing rule")?),
        tenant_id: TenantId::new(parse_floor_ulid(&row.tenant_id, "tenant")?),
        store_id: StoreId::new(parse_floor_ulid(&row.store_id, "store")?),
        station_id: StationId::new(parse_floor_ulid(&row.station_id, "station")?),
        menu_item_id,
        course_id,
        sort: u16::try_from(row.sort).unwrap_or(0),
    })
}

impl StationStore for PostgresFloor {
    async fn create(&self, station: &NewStation) -> Result<Version, FloorStoreError> {
        let backup = station.backup_station_id.map(|id| id.to_string());
        self.insert_station(
            &station.station_id.to_string(),
            &station.tenant_id.to_string(),
            &station.store_id.to_string(),
            &station.name,
            backup.as_deref(),
            station.is_default,
        )
        .await
        .map(Version::new)
        .map_err(|error| FloorStoreError::new(error.to_string()))
    }

    async fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Versioned<Station>>, FloorStoreError> {
        let rows = self
            .fetch_stations(&tenant.to_string(), &store_id.to_string())
            .await
            .map_err(|error| FloorStoreError::new(error.to_string()))?;
        rows.into_iter().map(station_record).collect()
    }

    async fn get(
        &self,
        tenant: TenantId,
        station_id: StationId,
    ) -> Result<Option<Versioned<Station>>, FloorStoreError> {
        let row = self
            .fetch_station(&tenant.to_string(), &station_id.to_string())
            .await
            .map_err(|error| FloorStoreError::new(error.to_string()))?;
        row.map(station_record).transpose()
    }

    async fn update(
        &self,
        station: &StationUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, FloorStoreError> {
        let backup = station.backup_station_id.map(|id| id.to_string());
        self.set_station(
            &station.tenant_id.to_string(),
            &station.station_id.to_string(),
            &station.name,
            backup.as_deref(),
            station.is_default,
            station.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| FloorStoreError::new(error.to_string()))
    }
}

impl RoutingRuleStore for PostgresFloor {
    async fn create(&self, rule: &NewRoutingRule) -> Result<(), FloorStoreError> {
        let menu_item = rule.menu_item_id.map(|id| id.to_string());
        let course = rule.course_id.map(|id| id.to_string());
        self.insert_rule(
            &rule.rule_id.to_string(),
            &rule.tenant_id.to_string(),
            &rule.store_id.to_string(),
            &rule.station_id.to_string(),
            menu_item.as_deref(),
            course.as_deref(),
            i32::from(rule.sort),
        )
        .await
        .map_err(|error| FloorStoreError::new(error.to_string()))
    }

    async fn list(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<RoutingRule>, FloorStoreError> {
        let rows = self
            .fetch_rules(&tenant.to_string(), &store_id.to_string())
            .await
            .map_err(|error| FloorStoreError::new(error.to_string()))?;
        rows.iter().map(routing_rule_record).collect()
    }

    async fn remove(
        &self,
        tenant: TenantId,
        rule_id: RoutingRuleId,
    ) -> Result<bool, FloorStoreError> {
        self.delete_rule(&tenant.to_string(), &rule_id.to_string())
            .await
            .map_err(|error| FloorStoreError::new(error.to_string()))
    }
}

// --- catalog (Phase 2a, ADR-0066): the store-postgres rows converted to the CatalogStore domain ---

fn parse_catalog_item_id(text: &str) -> Result<MenuItemId, CatalogStoreError> {
    text.parse::<Ulid>()
        .map(MenuItemId::new)
        .map_err(|_ignored| {
            CatalogStoreError::new(format!("a catalog item id is not a ULID: {text}"))
        })
}

fn parse_catalog_menu_id(text: &str) -> Result<MenuId, CatalogStoreError> {
    text.parse::<Ulid>().map(MenuId::new).map_err(|_ignored| {
        CatalogStoreError::new(format!("a catalog menu id is not a ULID: {text}"))
    })
}

fn parse_catalog_menu_section_id(text: &str) -> Result<MenuSectionId, CatalogStoreError> {
    text.parse::<Ulid>()
        .map(MenuSectionId::new)
        .map_err(|_ignored| {
            CatalogStoreError::new(format!("a catalog menu section id is not a ULID: {text}"))
        })
}

fn parse_catalog_tax_class(text: &str) -> Result<TaxClassId, CatalogStoreError> {
    text.parse::<Ulid>()
        .map(TaxClassId::new)
        .map_err(|_ignored| {
            CatalogStoreError::new(format!("a catalog tax class id is not a ULID: {text}"))
        })
}

fn parse_catalog_category_id(text: &str) -> Result<ItemCategoryId, CatalogStoreError> {
    text.parse::<Ulid>()
        .map(ItemCategoryId::new)
        .map_err(|_ignored| {
            CatalogStoreError::new(format!("a catalog item category id is not a ULID: {text}"))
        })
}

fn parse_catalog_subcategory_id(text: &str) -> Result<ItemSubcategoryId, CatalogStoreError> {
    text.parse::<Ulid>()
        .map(ItemSubcategoryId::new)
        .map_err(|_ignored| {
            CatalogStoreError::new(format!(
                "a catalog item sub-category id is not a ULID: {text}"
            ))
        })
}

fn catalog_item_record(row: CatalogItemRow) -> Result<Versioned<CatalogItem>, CatalogStoreError> {
    let item_category_id = match row.item_category_id {
        Some(text) => Some(parse_catalog_category_id(&text)?),
        None => None,
    };
    let item_subcategory_id = match row.item_subcategory_id {
        Some(text) => Some(parse_catalog_subcategory_id(&text)?),
        None => None,
    };
    // The `name_translations` jsonb is a locale→name object we wrote ourselves; a value that does not
    // parse as such falls back to no translations (the item's `name` remains its caption) rather than
    // failing the whole list — a malformed blob must not take a store's menu away.
    let name_translations: BTreeMap<String, String> =
        serde_json::from_str(&row.name_translations).unwrap_or_default();
    // A malformed `image_ref` (not a ULID) degrades to "no image" rather than failing the list — the
    // never-blank / placeholder posture (ADR-0075), the same as a media asset that was later deleted.
    let image_ref = row
        .image_ref
        .as_deref()
        .and_then(|text| text.parse::<Ulid>().ok())
        .map(MediaId::new);
    let version = Version::new(row.version);
    Ok(Versioned::new(
        CatalogItem {
            menu_item_id: parse_catalog_item_id(&row.menu_item_id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            name: row.name,
            name_translations,
            tax_class_id: parse_catalog_tax_class(&row.tax_class_id)?,
            item_category_id,
            item_subcategory_id,
            image_ref,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn catalog_category_record(
    row: CatalogTaxonomyRow,
) -> Result<Versioned<ItemCategory>, CatalogStoreError> {
    let version = Version::new(row.version);
    Ok(Versioned::new(
        ItemCategory {
            item_category_id: parse_catalog_category_id(&row.id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            name: row.name,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn catalog_subcategory_record(
    row: CatalogTaxonomyRow,
) -> Result<Versioned<ItemSubcategory>, CatalogStoreError> {
    let parent = row.parent_id.ok_or_else(|| {
        CatalogStoreError::new("an item sub-category row is missing its parent category")
    })?;
    let version = Version::new(row.version);
    Ok(Versioned::new(
        ItemSubcategory {
            item_subcategory_id: parse_catalog_subcategory_id(&row.id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            item_category_id: parse_catalog_category_id(&parent)?,
            name: row.name,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn catalog_tax_class_record(
    row: CatalogTaxClassRow,
) -> Result<Versioned<TaxClass>, CatalogStoreError> {
    let version = Version::new(row.version);
    Ok(Versioned::new(
        TaxClass {
            tax_class_id: parse_catalog_tax_class(&row.tax_class_id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            name: row.name,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn parse_catalog_display_category(text: &str) -> Result<DisplayCategoryId, CatalogStoreError> {
    text.parse::<Ulid>()
        .map(DisplayCategoryId::new)
        .map_err(|_ignored| {
            CatalogStoreError::new(format!(
                "a catalog display category id is not a ULID: {text}"
            ))
        })
}

fn parse_catalog_display_subcategory(
    text: &str,
) -> Result<DisplaySubcategoryId, CatalogStoreError> {
    text.parse::<Ulid>()
        .map(DisplaySubcategoryId::new)
        .map_err(|_ignored| {
            CatalogStoreError::new(format!(
                "a catalog display sub-category id is not a ULID: {text}"
            ))
        })
}

fn catalog_display_category_record(
    row: CatalogTaxonomyRow,
) -> Result<Versioned<DisplayCategory>, CatalogStoreError> {
    let version = Version::new(row.version);
    Ok(Versioned::new(
        DisplayCategory {
            display_category_id: parse_catalog_display_category(&row.id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            name: row.name,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn catalog_display_subcategory_record(
    row: CatalogTaxonomyRow,
) -> Result<Versioned<DisplaySubcategory>, CatalogStoreError> {
    let parent = row.parent_id.ok_or_else(|| {
        CatalogStoreError::new("a display sub-category row is missing its parent category")
    })?;
    let version = Version::new(row.version);
    Ok(Versioned::new(
        DisplaySubcategory {
            display_subcategory_id: parse_catalog_display_subcategory(&row.id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            display_category_id: parse_catalog_display_category(&parent)?,
            name: row.name,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn catalog_layout_button_record(
    row: CatalogLayoutButtonRow,
) -> Result<Versioned<LayoutButton>, CatalogStoreError> {
    let display_subcategory_id = match row.display_subcategory_id {
        Some(text) => Some(parse_catalog_display_subcategory(&text)?),
        None => None,
    };
    // A grid slot exists only when both column and row are stored; a flowing button has neither.
    let position = match (row.grid_column, row.grid_row) {
        (Some(column), Some(grid_row)) => {
            let column = u16::try_from(column).map_err(|_ignored| {
                CatalogStoreError::new("a layout button's grid column is out of range")
            })?;
            let row_index = u16::try_from(grid_row).map_err(|_ignored| {
                CatalogStoreError::new("a layout button's grid row is out of range")
            })?;
            Some(GridPosition {
                column,
                row: row_index,
            })
        }
        _ => None,
    };
    let version = Version::new(row.version);
    Ok(Versioned::new(
        LayoutButton {
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            sales_channel: Open::<SalesChannel>::parse(&row.sales_channel),
            display_category_id: parse_catalog_display_category(&row.display_category_id)?,
            display_subcategory_id,
            menu_item_id: parse_catalog_item_id(&row.menu_item_id)?,
            label: row.label,
            position,
            sort: row.sort,
        },
        version,
    ))
}

fn item_id_list_json(ids: &[MenuItemId]) -> Result<String, CatalogStoreError> {
    let raw: Vec<String> = ids.iter().map(ToString::to_string).collect();
    serde_json::to_string(&raw).map_err(|error| {
        CatalogStoreError::new(format!("could not serialise an item id list: {error}"))
    })
}

fn parse_catalog_item_id_list(json: &str) -> Result<Vec<MenuItemId>, CatalogStoreError> {
    let raw: Vec<String> = serde_json::from_str(json).map_err(|error| {
        CatalogStoreError::new(format!(
            "a modifier group's item id list is not valid JSON: {error}"
        ))
    })?;
    raw.iter().map(|text| parse_catalog_item_id(text)).collect()
}

fn catalog_modifier_group_record(
    row: CatalogModifierGroupRow,
) -> Result<Versioned<ModifierGroup>, CatalogStoreError> {
    let min_select = u16::try_from(row.min_select).map_err(|_ignored| {
        CatalogStoreError::new("a modifier group's min_select is out of range")
    })?;
    let max_select = u16::try_from(row.max_select).map_err(|_ignored| {
        CatalogStoreError::new("a modifier group's max_select is out of range")
    })?;
    let version = Version::new(row.version);
    Ok(Versioned::new(
        ModifierGroup {
            modifier_group_id: row
                .modifier_group_id
                .parse::<Ulid>()
                .map(ModifierGroupId::new)
                .map_err(|_ignored| {
                    CatalogStoreError::new(format!(
                        "a modifier group id is not a ULID: {}",
                        row.modifier_group_id
                    ))
                })?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            name: row.name,
            min_select,
            max_select,
            member_item_ids: parse_catalog_item_id_list(&row.member_item_ids_json)?,
            attached_item_ids: parse_catalog_item_id_list(&row.attached_item_ids_json)?,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn catalog_menu_record(row: CatalogMenuRow) -> Result<Versioned<Menu>, CatalogStoreError> {
    let parent_menu_id = match row.parent_menu_id {
        Some(text) => Some(parse_catalog_menu_id(&text)?),
        None => None,
    };
    let version = Version::new(row.version);
    Ok(Versioned::new(
        Menu {
            menu_id: parse_catalog_menu_id(&row.menu_id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            name: row.name,
            parent_menu_id,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn catalog_menu_section_record(
    row: CatalogMenuSectionRow,
) -> Result<Versioned<MenuSection>, CatalogStoreError> {
    let version = Version::new(row.version);
    Ok(Versioned::new(
        MenuSection {
            menu_section_id: parse_catalog_menu_section_id(&row.menu_section_id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            menu_id: parse_catalog_menu_id(&row.menu_id)?,
            name: row.name,
            sort: row.sort,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn catalog_placement_record(
    row: &CatalogPlacementRow,
) -> Result<Versioned<MenuPlacement>, CatalogStoreError> {
    let prices: Vec<ChannelPrice> = serde_json::from_str(&row.prices_json).map_err(|error| {
        CatalogStoreError::new(format!(
            "a placement's stored prices are not valid JSON: {error}"
        ))
    })?;
    let menu_section_id = match &row.menu_section_id {
        Some(text) => Some(parse_catalog_menu_section_id(text)?),
        None => None,
    };
    Ok(Versioned::new(
        MenuPlacement {
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            menu_id: parse_catalog_menu_id(&row.menu_id)?,
            menu_item_id: parse_catalog_item_id(&row.menu_item_id)?,
            menu_section_id,
            prices,
            available: row.available,
        },
        Version::new(row.version.clone()),
    ))
}

impl CatalogStore for PostgresCatalog {
    async fn create_item(&self, item: &CatalogItem) -> Result<Version, CatalogStoreError> {
        let category = item.item_category_id.map(|id| id.to_string());
        let subcategory = item.item_subcategory_id.map(|id| id.to_string());
        let name_translations = serde_json::to_string(&item.name_translations)
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        let image_ref = item.image_ref.map(|id| id.to_string());
        self.insert_item(
            &item.menu_item_id.to_string(),
            &item.tenant_id.to_string(),
            &item.name,
            &name_translations,
            &item.tax_class_id.to_string(),
            category.as_deref(),
            subcategory.as_deref(),
            image_ref.as_deref(),
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_items(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<CatalogItem>>, CatalogStoreError> {
        let rows = self
            .fetch_items(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_item_record).collect()
    }

    async fn list_items_page(
        &self,
        tenant_id: TenantId,
        page: PageRequest,
        filter: &ItemListFilter,
    ) -> Result<Page<Versioned<CatalogItem>>, CatalogStoreError> {
        // The one place the domain's sort vocabulary meets the adapter's. Exhaustive by necessity:
        // `clippy::wildcard_enum_match_arm` is denied, so a new `ItemSort` variant fails to compile
        // here rather than quietly falling back to the newest order.
        let order = match filter.sort {
            ItemSort::Newest => ItemOrder::Newest,
            ItemSort::Name => ItemOrder::Name,
            ItemSort::Status => ItemOrder::Status,
        };
        let (rows, total) = self
            .fetch_items_page(
                &tenant_id.to_string(),
                filter.search.as_deref(),
                order,
                filter.descending,
                i64::from(page.limit()),
                i64::from(page.offset()),
            )
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        let items: Vec<Versioned<CatalogItem>> = rows
            .into_iter()
            .map(catalog_item_record)
            .collect::<Result<_, _>>()?;
        Ok(Page::new(items, u32::try_from(total).unwrap_or(u32::MAX)))
    }

    async fn update_item(
        &self,
        item: &CatalogItem,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let category = item.item_category_id.map(|id| id.to_string());
        let subcategory = item.item_subcategory_id.map(|id| id.to_string());
        let name_translations = serde_json::to_string(&item.name_translations)
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        let image_ref = item.image_ref.map(|id| id.to_string());
        self.set_item(
            &item.tenant_id.to_string(),
            &item.menu_item_id.to_string(),
            &item.name,
            &name_translations,
            &item.tax_class_id.to_string(),
            category.as_deref(),
            subcategory.as_deref(),
            image_ref.as_deref(),
            item.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_tax_class(&self, tax_class: &TaxClass) -> Result<Version, CatalogStoreError> {
        self.insert_tax_class(
            &tax_class.tax_class_id.to_string(),
            &tax_class.tenant_id.to_string(),
            &tax_class.name,
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_tax_classes(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<TaxClass>>, CatalogStoreError> {
        let rows = self
            .fetch_tax_classes(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_tax_class_record).collect()
    }

    async fn update_tax_class(
        &self,
        tax_class: &TaxClass,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        self.set_tax_class(
            &tax_class.tenant_id.to_string(),
            &tax_class.tax_class_id.to_string(),
            &tax_class.name,
            tax_class.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_item_category(
        &self,
        category: &ItemCategory,
    ) -> Result<Version, CatalogStoreError> {
        self.insert_item_category(
            &category.item_category_id.to_string(),
            &category.tenant_id.to_string(),
            &category.name,
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_item_categories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<ItemCategory>>, CatalogStoreError> {
        let rows = self
            .fetch_item_categories(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_category_record).collect()
    }

    async fn update_item_category(
        &self,
        category: &ItemCategory,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        self.set_item_category(
            &category.tenant_id.to_string(),
            &category.item_category_id.to_string(),
            &category.name,
            category.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_item_subcategory(
        &self,
        subcategory: &ItemSubcategory,
    ) -> Result<Version, CatalogStoreError> {
        self.insert_item_subcategory(
            &subcategory.item_subcategory_id.to_string(),
            &subcategory.tenant_id.to_string(),
            &subcategory.item_category_id.to_string(),
            &subcategory.name,
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_item_subcategories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<ItemSubcategory>>, CatalogStoreError> {
        let rows = self
            .fetch_item_subcategories(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_subcategory_record).collect()
    }

    async fn update_item_subcategory(
        &self,
        subcategory: &ItemSubcategory,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        self.set_item_subcategory(
            &subcategory.tenant_id.to_string(),
            &subcategory.item_subcategory_id.to_string(),
            &subcategory.item_category_id.to_string(),
            &subcategory.name,
            subcategory.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_display_category(
        &self,
        category: &DisplayCategory,
    ) -> Result<Version, CatalogStoreError> {
        self.insert_display_category(
            &category.display_category_id.to_string(),
            &category.tenant_id.to_string(),
            &category.name,
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_display_categories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<DisplayCategory>>, CatalogStoreError> {
        let rows = self
            .fetch_display_categories(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter()
            .map(catalog_display_category_record)
            .collect()
    }

    async fn update_display_category(
        &self,
        category: &DisplayCategory,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        self.set_display_category(
            &category.tenant_id.to_string(),
            &category.display_category_id.to_string(),
            &category.name,
            category.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_display_subcategory(
        &self,
        subcategory: &DisplaySubcategory,
    ) -> Result<Version, CatalogStoreError> {
        self.insert_display_subcategory(
            &subcategory.display_subcategory_id.to_string(),
            &subcategory.tenant_id.to_string(),
            &subcategory.display_category_id.to_string(),
            &subcategory.name,
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_display_subcategories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<DisplaySubcategory>>, CatalogStoreError> {
        let rows = self
            .fetch_display_subcategories(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter()
            .map(catalog_display_subcategory_record)
            .collect()
    }

    async fn update_display_subcategory(
        &self,
        subcategory: &DisplaySubcategory,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        self.set_display_subcategory(
            &subcategory.tenant_id.to_string(),
            &subcategory.display_subcategory_id.to_string(),
            &subcategory.display_category_id.to_string(),
            &subcategory.name,
            subcategory.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_layout_button(
        &self,
        button: &LayoutButton,
    ) -> Result<CreateOutcome, CatalogStoreError> {
        let subcategory = button.display_subcategory_id.map(|id| id.to_string());
        self.insert_layout_button(
            &button.tenant_id.to_string(),
            button.sales_channel.as_wire(),
            &button.display_category_id.to_string(),
            subcategory.as_deref(),
            &button.menu_item_id.to_string(),
            &button.label,
            button.position.map(|p| i32::from(p.column)),
            button.position.map(|p| i32::from(p.row)),
            button.sort,
        )
        .await
        .map(create_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn update_layout_button(
        &self,
        button: &LayoutButton,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let subcategory = button.display_subcategory_id.map(|id| id.to_string());
        self.update_layout_button_at(
            &button.tenant_id.to_string(),
            button.sales_channel.as_wire(),
            &button.display_category_id.to_string(),
            subcategory.as_deref(),
            &button.menu_item_id.to_string(),
            &button.label,
            button.position.map(|p| i32::from(p.column)),
            button.position.map(|p| i32::from(p.row)),
            button.sort,
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_layout_buttons(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<LayoutButton>>, CatalogStoreError> {
        let rows = self
            .fetch_layout_buttons(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_layout_button_record).collect()
    }

    async fn remove_layout_button(
        &self,
        tenant_id: TenantId,
        sales_channel: Open<SalesChannel>,
        menu_item_id: MenuItemId,
    ) -> Result<bool, CatalogStoreError> {
        self.delete_layout_button(
            &tenant_id.to_string(),
            sales_channel.as_wire(),
            &menu_item_id.to_string(),
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_modifier_group(
        &self,
        group: &ModifierGroup,
    ) -> Result<Version, CatalogStoreError> {
        self.insert_modifier_group(
            &group.modifier_group_id.to_string(),
            &group.tenant_id.to_string(),
            &group.name,
            i32::from(group.min_select),
            i32::from(group.max_select),
            &item_id_list_json(&group.member_item_ids)?,
            &item_id_list_json(&group.attached_item_ids)?,
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_modifier_groups(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<ModifierGroup>>, CatalogStoreError> {
        let rows = self
            .fetch_modifier_groups(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter()
            .map(catalog_modifier_group_record)
            .collect()
    }

    async fn update_modifier_group(
        &self,
        group: &ModifierGroup,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        self.set_modifier_group(
            &group.tenant_id.to_string(),
            &group.modifier_group_id.to_string(),
            &group.name,
            i32::from(group.min_select),
            i32::from(group.max_select),
            &item_id_list_json(&group.member_item_ids)?,
            &item_id_list_json(&group.attached_item_ids)?,
            group.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_menu(&self, menu: &Menu) -> Result<Version, CatalogStoreError> {
        let parent = menu.parent_menu_id.map(|id| id.to_string());
        self.insert_menu(
            &menu.menu_id.to_string(),
            &menu.tenant_id.to_string(),
            &menu.name,
            parent.as_deref(),
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_menus(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<Menu>>, CatalogStoreError> {
        let rows = self
            .fetch_menus(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_menu_record).collect()
    }

    async fn update_menu(
        &self,
        menu: &Menu,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let parent = menu.parent_menu_id.map(|id| id.to_string());
        self.set_menu(
            &menu.tenant_id.to_string(),
            &menu.menu_id.to_string(),
            &menu.name,
            parent.as_deref(),
            menu.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_menu_section(
        &self,
        section: &MenuSection,
    ) -> Result<Version, CatalogStoreError> {
        self.insert_menu_section(
            &section.menu_section_id.to_string(),
            &section.tenant_id.to_string(),
            &section.menu_id.to_string(),
            &section.name,
            section.sort,
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_menu_sections(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
    ) -> Result<Vec<Versioned<MenuSection>>, CatalogStoreError> {
        let rows = self
            .fetch_menu_sections(&tenant_id.to_string(), &menu_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_menu_section_record).collect()
    }

    async fn update_menu_section(
        &self,
        section: &MenuSection,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        self.set_menu_section(
            &section.tenant_id.to_string(),
            &section.menu_section_id.to_string(),
            &section.name,
            section.sort,
            section.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_placement(
        &self,
        placement: &MenuPlacement,
    ) -> Result<CreateOutcome, CatalogStoreError> {
        let prices_json = serde_json::to_string(&placement.prices).map_err(|error| {
            CatalogStoreError::new(format!("could not serialise placement prices: {error}"))
        })?;
        let section = placement.menu_section_id.map(|id| id.to_string());
        self.insert_placement(
            &placement.tenant_id.to_string(),
            &placement.menu_id.to_string(),
            &placement.menu_item_id.to_string(),
            section.as_deref(),
            &prices_json,
            placement.available,
        )
        .await
        .map(create_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn update_placement(
        &self,
        placement: &MenuPlacement,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let prices_json = serde_json::to_string(&placement.prices).map_err(|error| {
            CatalogStoreError::new(format!("could not serialise placement prices: {error}"))
        })?;
        let section = placement.menu_section_id.map(|id| id.to_string());
        self.update_placement_at(
            &placement.tenant_id.to_string(),
            &placement.menu_id.to_string(),
            &placement.menu_item_id.to_string(),
            section.as_deref(),
            &prices_json,
            placement.available,
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_placements(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
    ) -> Result<Vec<Versioned<MenuPlacement>>, CatalogStoreError> {
        let rows = self
            .fetch_placements(&tenant_id.to_string(), &menu_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.iter().map(catalog_placement_record).collect()
    }

    async fn remove_placement(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
        menu_item_id: MenuItemId,
    ) -> Result<bool, CatalogStoreError> {
        self.delete_placement(
            &tenant_id.to_string(),
            &menu_id.to_string(),
            &menu_item_id.to_string(),
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }
}

/// The OTA release registry over PostgreSQL ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md),
/// roadmap-v3 slice R2).
///
/// The immutability rule is [`admit_artifact`]'s, consulted here with the digest the table already
/// holds — so the registry and the rule cannot drift, and the refusal a re-upload gets is the one the
/// unit tests pin rather than a second copy of it written in SQL.
impl ReleaseStore for PostgresReleases {
    async fn record_artifact(
        &self,
        artifact: &ReleaseArtifact,
    ) -> Result<RecordOutcome, ReleaseStoreError> {
        let target = artifact.target.to_string();
        let stored = self
            .stored_digest(&artifact.release, &target)
            .await
            .map_err(|error| ReleaseStoreError::unavailable(error.to_string()))?;
        let outcome = admit_artifact(stored.as_deref(), artifact)?;
        if outcome == RecordOutcome::AlreadyRecorded {
            return Ok(outcome);
        }
        self.insert_artifact(&ReleaseArtifactRow {
            release: artifact.release.clone(),
            target,
            size_bytes: artifact.size_bytes,
            sha256: artifact.sha256.clone(),
            recorded_at: artifact.recorded_at.as_milliseconds_since_epoch(),
        })
        .await
        .map_err(|error| ReleaseStoreError::unavailable(error.to_string()))?;
        Ok(outcome)
    }

    async fn find_artifact(
        &self,
        release: &str,
        target: &TargetTriple,
    ) -> Result<Option<ReleaseArtifact>, ReleaseStoreError> {
        // Fully qualified deliberately: the inherent method and this trait method share a name, and
        // resolution silently prefers the inherent one. Spelling it out means renaming the inherent
        // method is a compile error rather than an infinite recursion.
        let row = PostgresReleases::find_artifact(self, release, target.as_str())
            .await
            .map_err(|error| ReleaseStoreError::unavailable(error.to_string()))?;
        row.map(release_artifact_from_row).transpose()
    }

    async fn list_artifacts(
        &self,
        release: &str,
    ) -> Result<Vec<ReleaseArtifact>, ReleaseStoreError> {
        // Fully qualified for the same reason, and more sharply: the inherent `list_artifacts` has
        // the *identical* signature to this one, so a bare `self.list_artifacts(release)` would
        // recurse forever the moment the inherent method went away.
        let rows = PostgresReleases::list_artifacts(self, release)
            .await
            .map_err(|error| ReleaseStoreError::unavailable(error.to_string()))?;
        rows.into_iter().map(release_artifact_from_row).collect()
    }
}

/// Rehydrates a stored row into a [`ReleaseArtifact`].
///
/// A target or timestamp that no longer parses means the row was written by something that did not
/// go through this seam, so it is reported as a registry failure rather than skipped — a release the
/// cloud cannot describe must not silently look like a release that was never uploaded.
fn release_artifact_from_row(
    row: ReleaseArtifactRow,
) -> Result<ReleaseArtifact, ReleaseStoreError> {
    let target = TargetTriple::parse(&row.target).map_err(|error| {
        ReleaseStoreError::unavailable(format!("a stored release target is invalid: {error}"))
    })?;
    let recorded_at = Timestamp::from_milliseconds_since_epoch(row.recorded_at).map_err(|_| {
        ReleaseStoreError::unavailable("a stored release recorded_at is out of range")
    })?;
    Ok(ReleaseArtifact {
        release: row.release,
        target,
        size_bytes: row.size_bytes,
        sha256: row.sha256,
        recorded_at,
    })
}
