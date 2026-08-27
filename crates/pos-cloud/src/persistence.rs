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
    AdminInviteRow, AdminSessionRow, AdminUserRow, BrandRow, CatalogItemRow,
    CatalogLayoutButtonRow, CatalogMenuRow, CatalogMenuSectionRow, CatalogModifierGroupRow,
    CatalogPlacementRow, CatalogTaxClassRow, CatalogTaxonomyRow, DeviceRow, FleetStoreRow,
    NewSessionRow, OrderQueueRow, PendingOrderRow, PostgresActivationCodes, PostgresAdmin,
    PostgresApiKeys, PostgresCatalog, PostgresConfigTrees, PostgresDeviceProposals, PostgresFleet,
    PostgresOrderQueue, PostgresReconcile, PostgresRegistry, PostgresRollups, PostgresStore,
    PostgresStoreDirectory, PostgresSubjects, PostgresTaskHealth, PostgresTranslations,
    PostgresWebhooks, StoreRow, TaskHealthRow, TenantRow,
};

use pos_ports::PortError;
use pos_proto::display::GridPosition;
use pos_proto::enums::SalesChannel;
use pos_proto::ids::{
    ConfigVersionId, DeviceId, DisplayCategoryId, DisplaySubcategoryId, EventId, MenuItemId,
    StoreId, SubjectId, TaxClassId, TenantId,
};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

use pos_core::activation::CodeStatus;

use crate::activation::{ActivationCodeStore, ActivationStoreError, DeviceCredential, IssuedCode};
use crate::auth::SuperAdminCredential;
use crate::auth::admin::{
    AdminCredential, AdminInvite, AdminRole, AdminStatus, AdminStore, AdminStoreError, AdminUser,
    LiveSession, NewAdminInvite, NewAdminSession, NewAdminUser, NewRecoveryCode, SessionSummary,
};
use crate::auth::apikey::{
    ApiKeyAdminStore, ApiKeyId, ApiKeyStore, ApiKeyStoreError, ApiKeySummary, StoredApiKey,
};
use crate::auth::totp::TotpSecret;
use crate::catalog::{
    CatalogItem, CatalogStore, CatalogStoreError, ChannelPrice, DisplayCategory,
    DisplaySubcategory, ItemCategory, ItemCategoryId, ItemSubcategory, ItemSubcategoryId,
    LayoutButton, Menu, MenuId, MenuPlacement, MenuSection, MenuSectionId, ModifierGroup,
    ModifierGroupId, TaxClass,
};
use crate::config_tree::{ConfigStoreError, ConfigTreeState, ConfigTreeStore};
use crate::dashboard::projection::{RollupError, RollupStore, StoredRollups};
use crate::dashboard::projector::StoreCatalog;
use crate::devices::{
    DeviceProposalError, DeviceProposalId, DeviceProposalStatus, DeviceProposalStore,
    DeviceProposalSummary, PersistedDeviceProposal,
};
use crate::fleet::{FleetRow, FleetStore, FleetStoreError};
use crate::health::{TaskHealth, TaskHealthError, TaskHealthStore};
use crate::orders::StoreDirectory;
use crate::reconcile::{ReconcileError, ReconcileStore};
use crate::registry::{
    BrandId, BrandRecord, DeviceRecord, EntityStatus, RegistryStore, RegistryStoreError,
    StoreRecord, TenantRecord,
};
use crate::relay::{
    OrderQueueId, OrderQueueStore, OrderRecord, OrderStatus, PendingOrder, QueuedOrderPayload,
    StoreOutcome,
};
use crate::retention::{RetentionError, SubjectRecord, SubjectStore};
use crate::translations::{TranslationGrid, TranslationStore, TranslationStoreError};
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
    ) -> Result<Option<ConfigTreeState>, ConfigStoreError> {
        match self
            .load_state(tenant, store)
            .await
            .map_err(|error| ConfigStoreError::new(error.to_string()))?
        {
            Some(json) => serde_json::from_str(&json).map(Some).map_err(|error| {
                ConfigStoreError::new(format!("decoding the stored config tree failed: {error}"))
            }),
            None => Ok(None),
        }
    }

    async fn save(
        &self,
        tenant: TenantId,
        store: StoreId,
        state: &ConfigTreeState,
    ) -> Result<(), ConfigStoreError> {
        let json = serde_json::to_string(state).map_err(|error| {
            ConfigStoreError::new(format!("encoding the config tree failed: {error}"))
        })?;
        self.save_state(tenant, store, &json)
            .await
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
    ) -> Result<(), ConfigStoreError> {
        self.record_heartbeat(tenant, store, seen_at.as_milliseconds_since_epoch())
            .await
            .map_err(|error| ConfigStoreError::new(error.to_string()))
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
        PostgresApiKeys::insert(
            self,
            &key.id.to_string(),
            &key.tenant_id.to_string(),
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
                status: row.status,
            })
            .collect())
    }

    async fn resolve(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        approved: bool,
    ) -> Result<bool, DeviceProposalError> {
        let status = if approved {
            DeviceProposalStatus::Approved
        } else {
            DeviceProposalStatus::Rejected
        };
        self.mark(&tenant.to_string(), &id.to_string(), status.as_wire())
            .await
            .map_err(|error| DeviceProposalError::new(error.to_string()))
    }
}

impl TranslationStore for PostgresTranslations {
    async fn load(
        &self,
        tenant: TenantId,
    ) -> Result<Option<TranslationGrid>, TranslationStoreError> {
        let json = self
            .load_grid(&tenant.to_string())
            .await
            .map_err(|error| TranslationStoreError::new(error.to_string()))?;
        match json {
            Some(text) => serde_json::from_str(&text).map(Some).map_err(|error| {
                TranslationStoreError::new(format!("decoding the stored grid failed: {error}"))
            }),
            None => Ok(None),
        }
    }

    async fn save(
        &self,
        tenant: TenantId,
        grid: &TranslationGrid,
    ) -> Result<(), TranslationStoreError> {
        let json = serde_json::to_string(grid).map_err(|error| {
            TranslationStoreError::new(format!("encoding the grid failed: {error}"))
        })?;
        self.save_grid(&tenant.to_string(), &json)
            .await
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

fn tenant_record(row: TenantRow) -> Result<TenantRecord, RegistryStoreError> {
    Ok(TenantRecord {
        tenant_id: parse_registry_tenant(&row.tenant_id)?,
        name: row.name,
        status: EntityStatus::from_db(&row.status),
    })
}

fn brand_record(row: BrandRow) -> Result<BrandRecord, RegistryStoreError> {
    Ok(BrandRecord {
        brand_id: parse_registry_brand(&row.brand_id)?,
        tenant_id: parse_registry_tenant(&row.tenant_id)?,
        name: row.name,
        status: EntityStatus::from_db(&row.status),
    })
}

fn store_record(row: StoreRow) -> Result<StoreRecord, RegistryStoreError> {
    let brand_id = match row.brand_id {
        Some(text) => Some(parse_registry_brand(&text)?),
        None => None,
    };
    Ok(StoreRecord {
        store_id: parse_registry_store(&row.store_id)?,
        tenant_id: parse_registry_tenant(&row.tenant_id)?,
        brand_id,
        name: row.name,
        status: EntityStatus::from_db(&row.status),
    })
}

fn device_record(row: DeviceRow) -> Result<DeviceRecord, RegistryStoreError> {
    Ok(DeviceRecord {
        device_id: parse_registry_device(&row.device_id)?,
        tenant_id: parse_registry_tenant(&row.tenant_id)?,
        store_id: parse_registry_store(&row.store_id)?,
        name: row.name,
        kind: row.kind,
        status: EntityStatus::from_db(&row.status),
    })
}

impl RegistryStore for PostgresRegistry {
    async fn create_tenant(&self, tenant: &TenantRecord) -> Result<(), RegistryStoreError> {
        self.insert_tenant(&tenant.tenant_id.to_string(), &tenant.name)
            .await
            .map_err(|error| RegistryStoreError::new(error.to_string()))
    }

    async fn list_tenants(&self) -> Result<Vec<TenantRecord>, RegistryStoreError> {
        let rows = self
            .fetch_tenants()
            .await
            .map_err(|error| RegistryStoreError::new(error.to_string()))?;
        rows.into_iter().map(tenant_record).collect()
    }

    async fn update_tenant(&self, tenant: &TenantRecord) -> Result<bool, RegistryStoreError> {
        self.set_tenant(
            &tenant.tenant_id.to_string(),
            &tenant.name,
            tenant.status.as_str(),
        )
        .await
        .map_err(|error| RegistryStoreError::new(error.to_string()))
    }

    async fn create_brand(&self, brand: &BrandRecord) -> Result<(), RegistryStoreError> {
        self.insert_brand(
            &brand.brand_id.to_string(),
            &brand.tenant_id.to_string(),
            &brand.name,
        )
        .await
        .map_err(|error| RegistryStoreError::new(error.to_string()))
    }

    async fn list_brands(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<BrandRecord>, RegistryStoreError> {
        let rows = self
            .fetch_brands(&tenant_id.to_string())
            .await
            .map_err(|error| RegistryStoreError::new(error.to_string()))?;
        rows.into_iter().map(brand_record).collect()
    }

    async fn update_brand(&self, brand: &BrandRecord) -> Result<bool, RegistryStoreError> {
        self.set_brand(
            &brand.tenant_id.to_string(),
            &brand.brand_id.to_string(),
            &brand.name,
            brand.status.as_str(),
        )
        .await
        .map_err(|error| RegistryStoreError::new(error.to_string()))
    }

    async fn create_store(&self, store: &StoreRecord) -> Result<(), RegistryStoreError> {
        let brand = store.brand_id.map(|brand_id| brand_id.to_string());
        self.insert_store(
            &store.store_id.to_string(),
            &store.tenant_id.to_string(),
            brand.as_deref(),
            &store.name,
        )
        .await
        .map_err(|error| RegistryStoreError::new(error.to_string()))
    }

    async fn list_stores(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<StoreRecord>, RegistryStoreError> {
        let rows = self
            .fetch_stores(&tenant_id.to_string())
            .await
            .map_err(|error| RegistryStoreError::new(error.to_string()))?;
        rows.into_iter().map(store_record).collect()
    }

    async fn update_store(&self, store: &StoreRecord) -> Result<bool, RegistryStoreError> {
        let brand = store.brand_id.map(|brand_id| brand_id.to_string());
        self.set_store(
            &store.tenant_id.to_string(),
            &store.store_id.to_string(),
            brand.as_deref(),
            &store.name,
            store.status.as_str(),
        )
        .await
        .map_err(|error| RegistryStoreError::new(error.to_string()))
    }

    async fn create_device(&self, device: &DeviceRecord) -> Result<(), RegistryStoreError> {
        self.insert_device(
            &device.device_id.to_string(),
            &device.tenant_id.to_string(),
            &device.store_id.to_string(),
            &device.name,
            &device.kind,
        )
        .await
        .map_err(|error| RegistryStoreError::new(error.to_string()))
    }

    async fn list_devices(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<DeviceRecord>, RegistryStoreError> {
        let rows = self
            .fetch_devices(&tenant_id.to_string(), &store_id.to_string())
            .await
            .map_err(|error| RegistryStoreError::new(error.to_string()))?;
        rows.into_iter().map(device_record).collect()
    }

    async fn update_device(&self, device: &DeviceRecord) -> Result<bool, RegistryStoreError> {
        self.set_device(
            &device.tenant_id.to_string(),
            &device.device_id.to_string(),
            &device.name,
            &device.kind,
            device.status.as_str(),
        )
        .await
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

fn catalog_item_record(row: CatalogItemRow) -> Result<CatalogItem, CatalogStoreError> {
    let item_category_id = match row.item_category_id {
        Some(text) => Some(parse_catalog_category_id(&text)?),
        None => None,
    };
    let item_subcategory_id = match row.item_subcategory_id {
        Some(text) => Some(parse_catalog_subcategory_id(&text)?),
        None => None,
    };
    Ok(CatalogItem {
        menu_item_id: parse_catalog_item_id(&row.menu_item_id)?,
        tenant_id: parse_registry_tenant(&row.tenant_id)
            .map_err(|error| CatalogStoreError::new(error.to_string()))?,
        name: row.name,
        tax_class_id: parse_catalog_tax_class(&row.tax_class_id)?,
        item_category_id,
        item_subcategory_id,
        status: EntityStatus::from_db(&row.status),
    })
}

fn catalog_category_record(row: CatalogTaxonomyRow) -> Result<ItemCategory, CatalogStoreError> {
    Ok(ItemCategory {
        item_category_id: parse_catalog_category_id(&row.id)?,
        tenant_id: parse_registry_tenant(&row.tenant_id)
            .map_err(|error| CatalogStoreError::new(error.to_string()))?,
        name: row.name,
        status: EntityStatus::from_db(&row.status),
    })
}

fn catalog_subcategory_record(
    row: CatalogTaxonomyRow,
) -> Result<ItemSubcategory, CatalogStoreError> {
    let parent = row.parent_id.ok_or_else(|| {
        CatalogStoreError::new("an item sub-category row is missing its parent category")
    })?;
    Ok(ItemSubcategory {
        item_subcategory_id: parse_catalog_subcategory_id(&row.id)?,
        tenant_id: parse_registry_tenant(&row.tenant_id)
            .map_err(|error| CatalogStoreError::new(error.to_string()))?,
        item_category_id: parse_catalog_category_id(&parent)?,
        name: row.name,
        status: EntityStatus::from_db(&row.status),
    })
}

fn catalog_tax_class_record(row: CatalogTaxClassRow) -> Result<TaxClass, CatalogStoreError> {
    Ok(TaxClass {
        tax_class_id: parse_catalog_tax_class(&row.tax_class_id)?,
        tenant_id: parse_registry_tenant(&row.tenant_id)
            .map_err(|error| CatalogStoreError::new(error.to_string()))?,
        name: row.name,
        status: EntityStatus::from_db(&row.status),
    })
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
) -> Result<DisplayCategory, CatalogStoreError> {
    Ok(DisplayCategory {
        display_category_id: parse_catalog_display_category(&row.id)?,
        tenant_id: parse_registry_tenant(&row.tenant_id)
            .map_err(|error| CatalogStoreError::new(error.to_string()))?,
        name: row.name,
        status: EntityStatus::from_db(&row.status),
    })
}

fn catalog_display_subcategory_record(
    row: CatalogTaxonomyRow,
) -> Result<DisplaySubcategory, CatalogStoreError> {
    let parent = row.parent_id.ok_or_else(|| {
        CatalogStoreError::new("a display sub-category row is missing its parent category")
    })?;
    Ok(DisplaySubcategory {
        display_subcategory_id: parse_catalog_display_subcategory(&row.id)?,
        tenant_id: parse_registry_tenant(&row.tenant_id)
            .map_err(|error| CatalogStoreError::new(error.to_string()))?,
        display_category_id: parse_catalog_display_category(&parent)?,
        name: row.name,
        status: EntityStatus::from_db(&row.status),
    })
}

fn catalog_layout_button_record(
    row: CatalogLayoutButtonRow,
) -> Result<LayoutButton, CatalogStoreError> {
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
    Ok(LayoutButton {
        tenant_id: parse_registry_tenant(&row.tenant_id)
            .map_err(|error| CatalogStoreError::new(error.to_string()))?,
        sales_channel: Open::<SalesChannel>::parse(&row.sales_channel),
        display_category_id: parse_catalog_display_category(&row.display_category_id)?,
        display_subcategory_id,
        menu_item_id: parse_catalog_item_id(&row.menu_item_id)?,
        label: row.label,
        position,
        sort: row.sort,
    })
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
) -> Result<ModifierGroup, CatalogStoreError> {
    let min_select = u16::try_from(row.min_select).map_err(|_ignored| {
        CatalogStoreError::new("a modifier group's min_select is out of range")
    })?;
    let max_select = u16::try_from(row.max_select).map_err(|_ignored| {
        CatalogStoreError::new("a modifier group's max_select is out of range")
    })?;
    Ok(ModifierGroup {
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
    })
}

fn catalog_menu_record(row: CatalogMenuRow) -> Result<Menu, CatalogStoreError> {
    let parent_menu_id = match row.parent_menu_id {
        Some(text) => Some(parse_catalog_menu_id(&text)?),
        None => None,
    };
    Ok(Menu {
        menu_id: parse_catalog_menu_id(&row.menu_id)?,
        tenant_id: parse_registry_tenant(&row.tenant_id)
            .map_err(|error| CatalogStoreError::new(error.to_string()))?,
        name: row.name,
        parent_menu_id,
        status: EntityStatus::from_db(&row.status),
    })
}

fn catalog_menu_section_record(
    row: CatalogMenuSectionRow,
) -> Result<MenuSection, CatalogStoreError> {
    Ok(MenuSection {
        menu_section_id: parse_catalog_menu_section_id(&row.menu_section_id)?,
        tenant_id: parse_registry_tenant(&row.tenant_id)
            .map_err(|error| CatalogStoreError::new(error.to_string()))?,
        menu_id: parse_catalog_menu_id(&row.menu_id)?,
        name: row.name,
        sort: row.sort,
        status: EntityStatus::from_db(&row.status),
    })
}

fn catalog_placement_record(row: &CatalogPlacementRow) -> Result<MenuPlacement, CatalogStoreError> {
    let prices: Vec<ChannelPrice> = serde_json::from_str(&row.prices_json).map_err(|error| {
        CatalogStoreError::new(format!(
            "a placement's stored prices are not valid JSON: {error}"
        ))
    })?;
    let menu_section_id = match &row.menu_section_id {
        Some(text) => Some(parse_catalog_menu_section_id(text)?),
        None => None,
    };
    Ok(MenuPlacement {
        tenant_id: parse_registry_tenant(&row.tenant_id)
            .map_err(|error| CatalogStoreError::new(error.to_string()))?,
        menu_id: parse_catalog_menu_id(&row.menu_id)?,
        menu_item_id: parse_catalog_item_id(&row.menu_item_id)?,
        menu_section_id,
        prices,
        available: row.available,
    })
}

impl CatalogStore for PostgresCatalog {
    async fn create_item(&self, item: &CatalogItem) -> Result<(), CatalogStoreError> {
        let category = item.item_category_id.map(|id| id.to_string());
        let subcategory = item.item_subcategory_id.map(|id| id.to_string());
        self.insert_item(
            &item.menu_item_id.to_string(),
            &item.tenant_id.to_string(),
            &item.name,
            &item.tax_class_id.to_string(),
            category.as_deref(),
            subcategory.as_deref(),
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_items(&self, tenant_id: TenantId) -> Result<Vec<CatalogItem>, CatalogStoreError> {
        let rows = self
            .fetch_items(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_item_record).collect()
    }

    async fn update_item(&self, item: &CatalogItem) -> Result<bool, CatalogStoreError> {
        let category = item.item_category_id.map(|id| id.to_string());
        let subcategory = item.item_subcategory_id.map(|id| id.to_string());
        self.set_item(
            &item.tenant_id.to_string(),
            &item.menu_item_id.to_string(),
            &item.name,
            &item.tax_class_id.to_string(),
            category.as_deref(),
            subcategory.as_deref(),
            item.status.as_str(),
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_tax_class(&self, tax_class: &TaxClass) -> Result<(), CatalogStoreError> {
        self.insert_tax_class(
            &tax_class.tax_class_id.to_string(),
            &tax_class.tenant_id.to_string(),
            &tax_class.name,
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_tax_classes(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<TaxClass>, CatalogStoreError> {
        let rows = self
            .fetch_tax_classes(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_tax_class_record).collect()
    }

    async fn update_tax_class(&self, tax_class: &TaxClass) -> Result<bool, CatalogStoreError> {
        self.set_tax_class(
            &tax_class.tenant_id.to_string(),
            &tax_class.tax_class_id.to_string(),
            &tax_class.name,
            tax_class.status.as_str(),
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_item_category(&self, category: &ItemCategory) -> Result<(), CatalogStoreError> {
        self.insert_item_category(
            &category.item_category_id.to_string(),
            &category.tenant_id.to_string(),
            &category.name,
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_item_categories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<ItemCategory>, CatalogStoreError> {
        let rows = self
            .fetch_item_categories(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_category_record).collect()
    }

    async fn update_item_category(
        &self,
        category: &ItemCategory,
    ) -> Result<bool, CatalogStoreError> {
        self.set_item_category(
            &category.tenant_id.to_string(),
            &category.item_category_id.to_string(),
            &category.name,
            category.status.as_str(),
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_item_subcategory(
        &self,
        subcategory: &ItemSubcategory,
    ) -> Result<(), CatalogStoreError> {
        self.insert_item_subcategory(
            &subcategory.item_subcategory_id.to_string(),
            &subcategory.tenant_id.to_string(),
            &subcategory.item_category_id.to_string(),
            &subcategory.name,
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_item_subcategories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<ItemSubcategory>, CatalogStoreError> {
        let rows = self
            .fetch_item_subcategories(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_subcategory_record).collect()
    }

    async fn update_item_subcategory(
        &self,
        subcategory: &ItemSubcategory,
    ) -> Result<bool, CatalogStoreError> {
        self.set_item_subcategory(
            &subcategory.tenant_id.to_string(),
            &subcategory.item_subcategory_id.to_string(),
            &subcategory.item_category_id.to_string(),
            &subcategory.name,
            subcategory.status.as_str(),
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_display_category(
        &self,
        category: &DisplayCategory,
    ) -> Result<(), CatalogStoreError> {
        self.insert_display_category(
            &category.display_category_id.to_string(),
            &category.tenant_id.to_string(),
            &category.name,
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_display_categories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<DisplayCategory>, CatalogStoreError> {
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
    ) -> Result<bool, CatalogStoreError> {
        self.set_display_category(
            &category.tenant_id.to_string(),
            &category.display_category_id.to_string(),
            &category.name,
            category.status.as_str(),
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_display_subcategory(
        &self,
        subcategory: &DisplaySubcategory,
    ) -> Result<(), CatalogStoreError> {
        self.insert_display_subcategory(
            &subcategory.display_subcategory_id.to_string(),
            &subcategory.tenant_id.to_string(),
            &subcategory.display_category_id.to_string(),
            &subcategory.name,
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_display_subcategories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<DisplaySubcategory>, CatalogStoreError> {
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
    ) -> Result<bool, CatalogStoreError> {
        self.set_display_subcategory(
            &subcategory.tenant_id.to_string(),
            &subcategory.display_subcategory_id.to_string(),
            &subcategory.display_category_id.to_string(),
            &subcategory.name,
            subcategory.status.as_str(),
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn set_layout_button(&self, button: &LayoutButton) -> Result<(), CatalogStoreError> {
        let subcategory = button.display_subcategory_id.map(|id| id.to_string());
        self.upsert_layout_button(
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
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_layout_buttons(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<LayoutButton>, CatalogStoreError> {
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

    async fn create_modifier_group(&self, group: &ModifierGroup) -> Result<(), CatalogStoreError> {
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
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_modifier_groups(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<ModifierGroup>, CatalogStoreError> {
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
    ) -> Result<bool, CatalogStoreError> {
        self.set_modifier_group(
            &group.tenant_id.to_string(),
            &group.modifier_group_id.to_string(),
            &group.name,
            i32::from(group.min_select),
            i32::from(group.max_select),
            &item_id_list_json(&group.member_item_ids)?,
            &item_id_list_json(&group.attached_item_ids)?,
            group.status.as_str(),
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_menu(&self, menu: &Menu) -> Result<(), CatalogStoreError> {
        let parent = menu.parent_menu_id.map(|id| id.to_string());
        self.insert_menu(
            &menu.menu_id.to_string(),
            &menu.tenant_id.to_string(),
            &menu.name,
            parent.as_deref(),
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_menus(&self, tenant_id: TenantId) -> Result<Vec<Menu>, CatalogStoreError> {
        let rows = self
            .fetch_menus(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_menu_record).collect()
    }

    async fn update_menu(&self, menu: &Menu) -> Result<bool, CatalogStoreError> {
        let parent = menu.parent_menu_id.map(|id| id.to_string());
        self.set_menu(
            &menu.tenant_id.to_string(),
            &menu.menu_id.to_string(),
            &menu.name,
            parent.as_deref(),
            menu.status.as_str(),
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_menu_section(&self, section: &MenuSection) -> Result<(), CatalogStoreError> {
        self.insert_menu_section(
            &section.menu_section_id.to_string(),
            &section.tenant_id.to_string(),
            &section.menu_id.to_string(),
            &section.name,
            section.sort,
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_menu_sections(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
    ) -> Result<Vec<MenuSection>, CatalogStoreError> {
        let rows = self
            .fetch_menu_sections(&tenant_id.to_string(), &menu_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_menu_section_record).collect()
    }

    async fn update_menu_section(&self, section: &MenuSection) -> Result<bool, CatalogStoreError> {
        self.set_menu_section(
            &section.tenant_id.to_string(),
            &section.menu_section_id.to_string(),
            &section.name,
            section.sort,
            section.status.as_str(),
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn set_placement(&self, placement: &MenuPlacement) -> Result<(), CatalogStoreError> {
        let prices_json = serde_json::to_string(&placement.prices).map_err(|error| {
            CatalogStoreError::new(format!("could not serialise placement prices: {error}"))
        })?;
        let section = placement.menu_section_id.map(|id| id.to_string());
        self.upsert_placement(
            &placement.tenant_id.to_string(),
            &placement.menu_id.to_string(),
            &placement.menu_item_id.to_string(),
            section.as_deref(),
            &prices_json,
            placement.available,
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_placements(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
    ) -> Result<Vec<MenuPlacement>, CatalogStoreError> {
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
