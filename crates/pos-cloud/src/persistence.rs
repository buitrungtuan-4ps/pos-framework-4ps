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
    AlertRow, AssignmentRow, CampaignRow, EmployeeOrder, EmployeeRow, FleetStoreRow,
    NewScheduledPublishRow, NewVoucherRow, OrderQueueRow, PendingOrderRow, PostgresAlerts,
    PostgresArtifacts, PostgresCampaigns, PostgresConfigTrees, PostgresFleet, PostgresOrderQueue,
    PostgresPeople, PostgresReasonCodes, PostgresReconcile, PostgresScheduledPublishes,
    PostgresStore, PostgresStoreDirectory, PostgresSubjects, PostgresTaskHealth, PostgresVouchers,
    PostgresWebhooks, ReasonCodeRow, ReleaseArtifactRow, RoleTemplateRow, ScheduledPublishRow,
    TaskHealthRow, VoucherRow,
};
use store_postgres::{ConnectionRow, PostgresConnections};
use store_postgres::{ExpiredArchiveRow, PostgresArchives, StoreArchiveRow};
use store_postgres::{NewReleaseRow, PostgresConfigReleases, ReleaseRow};
use store_postgres::{PeopleRepublishRow, PostgresPeopleRepublishes};
use store_postgres::{PostgresStoreGroups, StoreGroupRow};

use store_postgres::{AnchorRow, ConflictRow, PostgresAnchors};

use sha2::{Digest as _, Sha256};

use crate::anchor::{
    AnchorConflict, AnchorError, AnchorLedger, ChainWindow, ConflictReport, HeldAnchors,
    StoreAnchor,
};
use crate::archive::{ArchiveStore, ArchiveStoreError, ExpiredArchive, StoreArchive};

use pos_ports::PortError;
use pos_ports::dynamic::BoxFuture;
use pos_proto::BusinessDate;
use pos_proto::campaign::PublishedCampaign;
use pos_proto::chain::ChainHash;
use pos_proto::enums::EdgePlacement;
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::error::ErrorStatus;
use pos_proto::ids::{CampaignId, EventId, ReasonCodeId, StoreId, SubjectId, TenantId};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::WireEnum;

use crate::alerts::{AlertKind, AlertRecord, AlertSeverity, AlertStore, AlertStoreError};
use crate::campaigns::{CampaignStore, CampaignStoreError};
use crate::cloud::{StoreOwner, StoreOwners};
use crate::dashboard::projector::StoreCatalog;
use pos_core::lease::LeaseGeneration;

use crate::connections::{Connection, ConnectionStore, ConnectionStoreError};
use crate::fleet::{FleetRow, FleetStore, FleetStoreError, OtaReportStore, PrintAgentStanding};
use crate::health::{TaskHealth, TaskHealthError, TaskHealthStore};
use crate::lease::{
    LeaseBump, LeaseBumpOutcome, LeaseStore, LeaseStoreError, Region, RegionAcknowledgement,
    RegionAcknowledgementError, RegionAcknowledgementStore, RegionWrite, RetireOutcome,
    SettleOutcome, StorePlacement,
};
use crate::orders::StoreDirectory;
use crate::ota::{
    ArtifactStore, ArtifactStoreError, RecordOutcome, ReleaseArtifact, TargetTriple, admit_artifact,
};
use crate::paging::{Page, PageRequest};
use crate::people::{
    Assignment, AssignmentId, AssignmentScope, AssignmentStore, AssignmentStoreError, Employee,
    EmployeeId, EmployeeListFilter, EmployeeSort, EmployeeStore, EmployeeStoreError,
    EmployeeUpdate, NewAssignment, NewEmployee, NewRoleTemplate, RoleTemplate, RoleTemplateId,
    RoleTemplateStore, RoleTemplateStoreError, RoleTemplateUpdate,
};
use crate::people_republish::{PeopleRepublish, PeopleRepublishError, PeopleRepublishStore};
use crate::reason_codes::{ReasonCodeStore, ReasonCodeStoreError};
use crate::reconcile::{ReconcileError, ReconcileRun, ReconcileRunStore, ReconcileStore};
use crate::registry::EntityStatus;
use crate::relay::{
    OrderQueueId, OrderQueueStore, OrderRecord, OrderStatus, PendingOrder, QueuedOrderPayload,
    StoreOutcome,
};
// This file holds both of the things "release" used to mean, and it is why the OTA half was
// renamed. The seam above is a version of the *software* — a signed edge binary and its artifact
// ([ADR-0048](../../../docs/adr/0048-ota-rollouts.md)) — and is now spelled `ArtifactStore`, which
// is what [ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md) calls the thing it stores. The
// seam below is a set of config nodes going out to a set of shops
// ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md)) and keeps the plain
// name, because among config it is the only release there is. Neither import needs an alias now.
use crate::releases::{
    NewRelease, Release, ReleaseMoment, ReleaseStatus, ReleaseStore, ReleaseStoreError,
};
use crate::retention::{RetentionError, SubjectRecord, SubjectStore};
use crate::scheduling::{
    NewScheduledPublish, ScheduledPublish, ScheduledPublishError, ScheduledPublishStatus,
    ScheduledPublishStore,
};
use crate::store_groups::{
    BatchOutcome, BatchResult, ConfigBatch, ConfigBatchId, StoreGroup, StoreGroupId,
    StoreGroupStore, StoreGroupStoreError,
};
use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned};
// The registry's id parsers and the conditional-write translations moved to pos-cloud-config with
// the registry's impl; every impl here still uses them.
use crate::vouchers::{NewVoucher, VoucherRecord, VoucherStatus, VoucherStore, VoucherStoreError};
use crate::webhook::sign::SigningSecret;
use crate::webhook::store::{
    PersistedWebhook, WebhookEndpointId, WebhookEndpointStore, WebhookStoreError, WebhookSummary,
};
use pos_cloud_config::persistence::{create_outcome, parse_registry_store, update_outcome};

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

impl RegionAcknowledgementStore for PostgresConfigTrees {
    /// Forwards to the adapter's upsert. The country codes go down as the two letters the column
    /// holds, through the same `CountryCode` the store's own region was written with, so the read's
    /// comparison is an equality test rather than a case fold
    /// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
    async fn record(
        &self,
        tenant: TenantId,
        store: StoreId,
        acknowledgement: &RegionAcknowledgement,
    ) -> Result<(), RegionAcknowledgementError> {
        self.record_region_acknowledgement(
            tenant,
            store,
            acknowledgement.profile_country().as_str(),
            acknowledgement.region_country().as_str(),
            acknowledgement.reason(),
            acknowledgement
                .acknowledged_at()
                .as_milliseconds_since_epoch(),
        )
        .await
        .map_err(|error| RegionAcknowledgementError(error.to_string()))
    }

    /// A row whose country codes this build cannot read, or whose stored instant is not a time,
    /// becomes `None` rather than an error — the same posture `Region::stored` takes one table over.
    /// An unreadable answer could not match the store's current pair anyway, so the warning it would
    /// have answered is already back, which is the honest outcome.
    async fn current(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> Result<Option<RegionAcknowledgement>, RegionAcknowledgementError> {
        let stored = self
            .region_acknowledgement(tenant, store)
            .await
            .map_err(|error| RegionAcknowledgementError(error.to_string()))?;
        Ok(stored.and_then(|stored| {
            let at = Timestamp::from_milliseconds_since_epoch(stored.acknowledged_at_ms).ok()?;
            RegionAcknowledgement::stored(
                &stored.profile_country,
                &stored.region_country,
                &stored.reason,
                at,
            )
        }))
    }
}

impl LeaseStore for PostgresConfigTrees {
    async fn highest_receipt_number(
        &self,
        _tenant: TenantId,
        store: StoreId,
    ) -> Result<Option<u64>, LeaseStoreError> {
        // Store ids are unique across tenants, and the stored tenant is the registry's stamp, so the
        // store alone names its log.
        let highest = PostgresConfigTrees::highest_receipt_number(self, store)
            .await
            .map_err(|error| LeaseStoreError::new(error.to_string()))?;
        Ok(highest.and_then(|number| u64::try_from(number).ok()))
    }

    /// Forwards to the adapter's single-statement bump, so two admins replacing a machine at once
    /// serialise on the row rather than racing to the same generation — and so the generation and
    /// the edge placement beside it are written by the same statement
    /// ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
    async fn bump(
        &self,
        tenant: TenantId,
        store: StoreId,
        issued_at: Timestamp,
        edge_placement: Option<EdgePlacement>,
        region: RegionWrite,
        acknowledge_undrained: Option<LeaseGeneration>,
        expected_generation: Option<LeaseGeneration>,
    ) -> Result<LeaseBumpOutcome, LeaseStoreError> {
        // The seam's three cases map one-for-one onto the adapter's, which is the point of both
        // being enums: there is no arm here that could construct a hosted placement with no region.
        let stored_region = match &region {
            RegionWrite::Keep => store_postgres::StoredRegionWrite::Keep,
            RegionWrite::Clear => store_postgres::StoredRegionWrite::Clear,
            RegionWrite::Set(region) => store_postgres::StoredRegionWrite::Set {
                country: region.country_str(),
                label: region.label(),
            },
        };
        let outcome = self
            .bump_store_lease(
                tenant,
                store,
                issued_at.as_milliseconds_since_epoch(),
                edge_placement.map(EdgePlacement::as_wire),
                stored_region,
                acknowledge_undrained.map(stored_generation_out),
                expected_generation.map(stored_generation_out),
            )
            .await
            .map_err(|error| LeaseStoreError::new(error.to_string()))?;
        let stored = match outcome {
            store_postgres::BumpOutcome::Issued(stored) => stored,
            store_postgres::BumpOutcome::VersionMismatch { current } => {
                return Ok(LeaseBumpOutcome::VersionMismatch {
                    current: current.map(stored_lease_generation).transpose()?,
                });
            }
            store_postgres::BumpOutcome::Undrained { superseded } => {
                return Ok(LeaseBumpOutcome::Undrained {
                    superseded: stored_lease_generation(superseded)?,
                });
            }
        };
        // Both columns or neither: the write sets and clears them together, so a row holding one
        // alone is a repair somebody made by hand. Reported as no region rather than as half of
        // one — `Region::stored` then declines a country that is not two letters, on the same
        // posture, because a fleet page must not fail over one bad row.
        let region = match (&stored.region_country, &stored.region_label) {
            (Some(country), Some(label)) => Region::stored(country, label),
            _ => None,
        };
        Ok(LeaseBumpOutcome::Issued(LeaseBump {
            generation: stored_lease_generation(stored.generation)?,
            edge_placement: stored_edge_placement(&stored.edge_placement)?,
            region,
            // Same refusal posture as the generation beside it, and for the same reason: a negative
            // number is not something this cloud ever wrote, so reporting a handover as settled
            // because a corrupt value would not convert is the one answer that must not be given.
            superseded_generation: stored
                .superseded_generation
                .map(stored_lease_generation)
                .transpose()?,
        }))
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

    async fn settle_handover(
        &self,
        tenant: TenantId,
        store: StoreId,
        superseded: LeaseGeneration,
    ) -> Result<SettleOutcome, LeaseStoreError> {
        let outcome = self
            .settle_handover(tenant, store, stored_generation_out(superseded))
            .await
            .map_err(|error| LeaseStoreError::new(error.to_string()))?;
        Ok(match outcome {
            store_postgres::StoredSettle::Settled => SettleOutcome::Settled,
            store_postgres::StoredSettle::NotSuperseded { current } => {
                SettleOutcome::NotSuperseded {
                    current: current.map(stored_lease_generation).transpose()?,
                }
            }
            store_postgres::StoredSettle::NoLease => SettleOutcome::NoLease,
        })
    }

    async fn retire(
        &self,
        tenant: TenantId,
        store: StoreId,
        retired_at: Timestamp,
        retired_by: &str,
    ) -> Result<RetireOutcome, LeaseStoreError> {
        let outcome = self
            .retire_handover(
                tenant,
                store,
                retired_at.as_milliseconds_since_epoch(),
                retired_by,
            )
            .await
            .map_err(|error| LeaseStoreError::new(error.to_string()))?;
        Ok(match outcome {
            store_postgres::StoredRetire::Retired => RetireOutcome::Retired,
            store_postgres::StoredRetire::Undrained { superseded } => RetireOutcome::Undrained {
                superseded: stored_lease_generation(superseded)?,
            },
            store_postgres::StoredRetire::AlreadyRetired { at, by } => {
                RetireOutcome::AlreadyRetired {
                    // A stored instant this cloud wrote itself. If it will not read back, the row
                    // has been tampered with or corrupted — the same posture the generation beside
                    // it takes, and for the same reason: inventing an instant would put a
                    // fabricated decision time in front of a person deciding what to switch off.
                    at: Timestamp::from_milliseconds_since_epoch(at).map_err(|error| {
                        LeaseStoreError::new(format!(
                            "the stored retirement instant is not a time this cloud writes: {error}"
                        ))
                    })?,
                    by,
                }
            }
            store_postgres::StoredRetire::NeverSuperseded => RetireOutcome::NeverSuperseded,
            store_postgres::StoredRetire::Raced => RetireOutcome::Raced,
            store_postgres::StoredRetire::NoLease => RetireOutcome::NoLease,
        })
    }
}

/// A domain generation as the `bigint` the column holds.
///
/// The counterpart to [`stored_lease_generation`]. Saturating rather than fallible because the
/// domain type is a `u64` and the column a signed 64-bit integer: a generation past `i64::MAX` is
/// unreachable in any fleet — it would take one bump per nanosecond for longer than the universe has
/// existed — and a saturating conversion keeps the caller free of an error arm nothing can produce.
fn stored_generation_out(generation: LeaseGeneration) -> i64 {
    i64::try_from(generation.value()).unwrap_or(i64::MAX)
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

/// Reads a stored `EDGE_PLACEMENT_*` token back into the domain's enum
/// ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
///
/// Refusing an unrecognised token rather than degrading it to `UNSPECIFIED` inverts the usual wire
/// rule on purpose, and the reason is which direction the value came from. Tolerating the unknown is
/// how a *receiver* stays compatible with a newer sender; this is a reader of its own database,
/// whose only writer is the statement two functions up, behind a `CHECK` naming the three tokens.
/// A value outside that set is a corrupt or tampered row, and the harm from carrying on is specific:
/// `UNSPECIFIED` renders as "we do not know where this store runs", which the console would show
/// beside "Offline-capable" for a store that is in fact hosted. Saying nothing beats saying the
/// wrong thing about whether a shop can trade with the line down.
fn stored_edge_placement(token: &str) -> Result<EdgePlacement, LeaseStoreError> {
    EdgePlacement::from_wire(token).ok_or_else(|| {
        LeaseStoreError::new(format!(
            "the stored edge placement is not one this build knows: {token}"
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

// --- The fleet read model (ADR-0068) -----------------------------------------------------------

/// Converts one joined `store-postgres` fleet row into the cloud's [`FleetRow`]. A liveness timestamp
/// out of range (impossible for values this cloud wrote) fails safe to "never seen" rather than
/// failing the whole listing; a negative backlog (impossible from a `count`) reads as zero.
/// What the fleet read can say about a store's placement, from the `LEFT JOIN`ed column.
///
/// Deliberately **not** [`stored_edge_placement`]'s posture. That decoder serves the *lease* path
/// and refuses an unrecognised token, because a bump that cannot name where it put a store should
/// fail rather than record a lie. This is the *fleet* path, and one corrupt row must not take down
/// the console an operator is holding open during an incident — so the row survives.
///
/// It survives as [`StorePlacement::Unreadable`], never as "no record". That distinction is the
/// whole point of the three-state type: `Unreadable` scores as "cannot trade with the line down",
/// the same as hosted, so a token this binary cannot read raises the severity rather than lowering
/// it. Collapsing it into `NeverIssued` would make a dark hosted store page as a warning.
fn fleet_placement(token: Option<&str>) -> StorePlacement {
    let Some(token) = token else {
        return StorePlacement::NeverIssued;
    };
    if let Some(placement) = EdgePlacement::from_wire(token) {
        return StorePlacement::Known(placement);
    }
    tracing::warn!(
        token = %token,
        "a store_lease row carries an edge_placement this build does not recognise; scoring it as \
         unreadable, which alerts at the hosted severity"
    );
    StorePlacement::Unreadable
}

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
    let retired_at = row
        .retired_at
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
        // Deliberately NOT `stored_edge_placement`'s posture. That decoder serves the *lease* path
        // and refuses an unrecognised token, because a bump that cannot name where it put a store
        // should fail rather than record a lie. This is the *fleet* path: one corrupt row must not
        // 503 the console an operator is holding open during an incident, so the row survives — but
        // it survives as `Unreadable`, never as "no record". Collapsing the two is the bug
        // `StorePlacement` exists to prevent: `Unreadable` scores as "cannot trade with the line
        // down", the same as hosted, so a token this binary cannot read pages at the higher
        // severity rather than the lower one.
        edge_placement: fleet_placement(row.edge_placement.as_deref()),
        // Both columns or neither: the bump writes and clears them together and no statement
        // touches one alone, so a row holding half a region is a repair somebody made by hand. It
        // reports as no region rather than as half of one, and `Region::stored` then declines a
        // country that is not two letters — the same posture as the placement above, for the same
        // reason: one bad row must not take down the console an operator is holding open.
        region: match (&row.region_country, &row.region_label) {
            (Some(country), Some(label)) => Region::stored(country, label),
            _ => None,
        },
        // Same narrowing rule as the generations above: a negative number cannot have been written
        // by this cloud, and "did not say" is a truer answer for a reader than a wrapped-around one.
        // Here it also fails in the safe direction — a store whose handover cannot be read reports
        // no handover, which is the state that shows *less* confidence rather than more.
        superseded_generation: row
            .superseded_generation
            .and_then(|value| u64::try_from(value).ok()),
        // An unreadable instant drops the retirement rather than inventing one, and that direction
        // is deliberate too: a handover reported as *not* retired is the reading that leaves an
        // operator looking at the machine, which is the safer mistake.
        retired_at,
        retired_by: row.retired_by,
        // A list this binary cannot parse is dropped rather than failing the fleet read, the same
        // posture `edge_placement` takes one field up: one corrupt row must not 503 a console an
        // operator is holding open during an incident. It degrades to "did not say", which shows
        // *less* confidence rather than more.
        print_agents: row
            .print_agents
            .as_deref()
            .and_then(|json| match serde_json::from_str::<Vec<PrintAgentStanding>>(json) {
                Ok(agents) => Some(agents),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "a stored print-agent standing could not be decoded; the fleet row reports none"
                    );
                    None
                }
            }),
        print_agents_reported_at: row
            .print_agents_reported_at_ms
            .and_then(|ms| Timestamp::from_milliseconds_since_epoch(ms).ok()),
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

/// Decodes one stored reason-code document into the wire record it is.
fn decode_reason_code(
    json: &str,
) -> Result<pos_proto::reason_codes::PublishedReasonCode, ReasonCodeStoreError> {
    serde_json::from_str(json).map_err(|error| {
        ReasonCodeStoreError::new(format!(
            "a stored reason code could not be decoded: {error}"
        ))
    })
}

/// Pairs a stored reason-code row with the version the read saw and the instant it was written.
fn versioned_reason_code(
    row: &ReasonCodeRow,
) -> Result<Versioned<pos_proto::reason_codes::PublishedReasonCode>, ReasonCodeStoreError> {
    Ok(Versioned::edited(
        decode_reason_code(&row.doc_json)?,
        Version::new(row.version.clone()),
        row.updated_at_ms,
    ))
}

impl ReasonCodeStore for PostgresReasonCodes {
    async fn list(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<pos_proto::reason_codes::PublishedReasonCode>>, ReasonCodeStoreError>
    {
        let rows = self
            .fetch(&tenant_id.to_string())
            .await
            .map_err(|error| ReasonCodeStoreError::new(error.to_string()))?;
        rows.iter().map(versioned_reason_code).collect()
    }

    async fn get(
        &self,
        tenant_id: TenantId,
        reason_code_id: ReasonCodeId,
    ) -> Result<Option<Versioned<pos_proto::reason_codes::PublishedReasonCode>>, ReasonCodeStoreError>
    {
        let row = self
            .fetch_one(&tenant_id.to_string(), &reason_code_id.to_string())
            .await
            .map_err(|error| ReasonCodeStoreError::new(error.to_string()))?;
        row.as_ref().map(versioned_reason_code).transpose()
    }

    async fn create(
        &self,
        tenant_id: TenantId,
        reason_code: &pos_proto::reason_codes::PublishedReasonCode,
    ) -> Result<CreateOutcome, ReasonCodeStoreError> {
        let json = serde_json::to_string(reason_code).map_err(|error| {
            ReasonCodeStoreError::new(format!("could not serialize a reason code: {error}"))
        })?;
        self.insert(&tenant_id.to_string(), &reason_code.id.to_string(), &json)
            .await
            .map(create_outcome)
            .map_err(|error| ReasonCodeStoreError::new(error.to_string()))
    }

    async fn update(
        &self,
        tenant_id: TenantId,
        reason_code: &pos_proto::reason_codes::PublishedReasonCode,
        expected: &Version,
    ) -> Result<UpdateOutcome, ReasonCodeStoreError> {
        let json = serde_json::to_string(reason_code).map_err(|error| {
            ReasonCodeStoreError::new(format!("could not serialize a reason code: {error}"))
        })?;
        self.update_at(
            &tenant_id.to_string(),
            &reason_code.id.to_string(),
            &json,
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| ReasonCodeStoreError::new(error.to_string()))
    }

    async fn delete(
        &self,
        tenant_id: TenantId,
        reason_code_id: ReasonCodeId,
    ) -> Result<(), ReasonCodeStoreError> {
        self.delete(&tenant_id.to_string(), &reason_code_id.to_string())
            .await
            .map_err(|error| ReasonCodeStoreError::new(error.to_string()))
    }
}

/// Pairs a stored connection row with the version the read saw and the instant it was written.
fn versioned_connection(
    row: &ConnectionRow,
) -> Result<Versioned<Connection>, ConnectionStoreError> {
    let connection = serde_json::from_str(&row.doc_json).map_err(|error| {
        ConnectionStoreError::new(format!("a stored connection could not be decoded: {error}"))
    })?;
    Ok(Versioned::edited(
        connection,
        Version::new(row.version.clone()),
        row.updated_at_ms,
    ))
}

/// Serializes a connection for storage. Its secrets are already sealed; nothing here sees a
/// plaintext one.
fn connection_doc(connection: &Connection) -> Result<String, ConnectionStoreError> {
    serde_json::to_string(connection).map_err(|error| {
        ConnectionStoreError::new(format!("could not serialize a connection: {error}"))
    })
}

impl ConnectionStore for PostgresConnections {
    async fn list(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<Connection>>, ConnectionStoreError> {
        let rows = self
            .fetch(&tenant_id.to_string())
            .await
            .map_err(|error| ConnectionStoreError::new(error.to_string()))?;
        rows.iter().map(versioned_connection).collect()
    }

    async fn get(
        &self,
        tenant_id: TenantId,
        connection_id: &str,
    ) -> Result<Option<Versioned<Connection>>, ConnectionStoreError> {
        let row = self
            .fetch_one(&tenant_id.to_string(), connection_id)
            .await
            .map_err(|error| ConnectionStoreError::new(error.to_string()))?;
        row.as_ref().map(versioned_connection).transpose()
    }

    async fn create(
        &self,
        tenant_id: TenantId,
        connection: &Connection,
    ) -> Result<CreateOutcome, ConnectionStoreError> {
        let json = connection_doc(connection)?;
        self.insert(&tenant_id.to_string(), &connection.connection_id, &json)
            .await
            .map(create_outcome)
            .map_err(|error| ConnectionStoreError::new(error.to_string()))
    }

    async fn update(
        &self,
        tenant_id: TenantId,
        connection: &Connection,
        expected: &Version,
    ) -> Result<UpdateOutcome, ConnectionStoreError> {
        let json = connection_doc(connection)?;
        self.update_at(
            &tenant_id.to_string(),
            &connection.connection_id,
            &json,
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| ConnectionStoreError::new(error.to_string()))
    }

    async fn delete(
        &self,
        tenant_id: TenantId,
        connection_id: &str,
    ) -> Result<(), ConnectionStoreError> {
        PostgresConnections::delete(self, &tenant_id.to_string(), connection_id)
            .await
            .map_err(|error| ConnectionStoreError::new(error.to_string()))
    }
}

/// Rehydrates a queued store, failing loudly on an id or a time this cloud wrote that no longer
/// parses — store corruption, not an absence — as a scheduled publish's row does.
fn republish_from_row(row: PeopleRepublishRow) -> Result<PeopleRepublish, PeopleRepublishError> {
    let tenant_id = row
        .tenant_id
        .parse::<Ulid>()
        .map(TenantId::new)
        .map_err(|_ignored| PeopleRepublishError::new("a queued store has a non-ULID tenant"))?;
    let store_id = row
        .store_id
        .parse::<Ulid>()
        .map(StoreId::new)
        .map_err(|_ignored| PeopleRepublishError::new("a queued store has a non-ULID id"))?;
    let enqueued_time = Timestamp::from_milliseconds_since_epoch(row.enqueued_time_ms)
        .map_err(|error| PeopleRepublishError::new(error.to_string()))?;
    Ok(PeopleRepublish {
        tenant_id,
        store_id,
        reason: row.reason,
        enqueued_time,
    })
}

impl PeopleRepublishStore for PostgresPeopleRepublishes {
    async fn enqueue(&self, republish: &PeopleRepublish) -> Result<(), PeopleRepublishError> {
        let row = PeopleRepublishRow {
            tenant_id: republish.tenant_id.to_string(),
            store_id: republish.store_id.to_string(),
            reason: republish.reason.clone(),
            enqueued_time_ms: republish.enqueued_time.as_milliseconds_since_epoch(),
        };
        Self::enqueue(self, &row)
            .await
            .map_err(|error| PeopleRepublishError::new(error.to_string()))
    }

    async fn pending(
        &self,
        after: Option<(TenantId, StoreId)>,
        limit: usize,
    ) -> Result<Vec<PeopleRepublish>, PeopleRepublishError> {
        let after =
            after.map(|(tenant_id, store_id)| (tenant_id.to_string(), store_id.to_string()));
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = Self::pending(
            self,
            after
                .as_ref()
                .map(|(tenant_id, store_id)| (tenant_id.as_str(), store_id.as_str())),
            limit,
        )
        .await
        .map_err(|error| PeopleRepublishError::new(error.to_string()))?;
        rows.into_iter().map(republish_from_row).collect()
    }

    async fn clear(&self, republish: &PeopleRepublish) -> Result<bool, PeopleRepublishError> {
        Self::clear(
            self,
            &republish.tenant_id.to_string(),
            &republish.store_id.to_string(),
            republish.enqueued_time.as_milliseconds_since_epoch(),
        )
        .await
        .map_err(|error| PeopleRepublishError::new(error.to_string()))
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
        release_id: row.release_id,
        failure: row.failure,
        resolved_timezone: row.resolved_timezone,
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
            release_id: publish.release_id.as_deref(),
            resolved_timezone: publish.resolved_timezone.as_deref(),
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

    async fn mark_failed(&self, id: &str, failure: &str) -> Result<(), ScheduledPublishError> {
        self.mark_failed(id, failure)
            .await
            .map_err(|error| ScheduledPublishError::new(error.to_string()))
    }

    async fn list_for_release(
        &self,
        tenant_id: TenantId,
        release_id: &str,
    ) -> Result<Vec<ScheduledPublish>, ScheduledPublishError> {
        let rows = self
            .list_for_release(&tenant_id.to_string(), release_id)
            .await
            .map_err(|error| ScheduledPublishError::new(error.to_string()))?;
        rows.into_iter().map(scheduled_from_row).collect()
    }
}

// --- Releases (ADR-0125) ------------------------------------------------------------------------

/// Rehydrates a stored release row into the seam's domain type.
///
/// The moment is reconstructed from the three columns rather than stored as one, and the
/// reconstruction is where the two kinds stay distinguishable: a wall-clock release carries a local
/// date and time whose instants live per store on its pairs; an instant release carries one moment
/// for the fleet. A row with neither is a `draft` nobody has timed yet, which is an ordinary state —
/// a release across nine nodes and forty stores is assembled over minutes.
///
/// A half-written wall clock (a date and no time, or the reverse) reads as *untimed* rather than as
/// a guess, because guessing midnight would schedule a publish at an hour nobody chose.
fn release_from_row(row: ReleaseRow) -> Result<Release, ReleaseStoreError> {
    let tenant_id = row
        .tenant_id
        .parse::<Ulid>()
        .map(TenantId::new)
        .map_err(|_ignored| ReleaseStoreError::new("a release has a non-ULID tenant"))?;
    let moment = match (row.wall_clock_date, row.wall_clock_time, row.instant_at_ms) {
        (Some(date), Some(time), _) => Some(ReleaseMoment::WallClock { date, time }),
        (_, _, Some(at_ms)) => Some(ReleaseMoment::Instant(at_ms)),
        _ => None,
    };
    Ok(Release {
        id: row.id,
        tenant_id,
        name: row.name,
        status: ReleaseStatus::from_wire(&row.status),
        target_group_id: row.target_group_id,
        moment,
        created_by: row.created_by,
        created_at_ms: row.created_at_ms,
        updated_at_ms: row.updated_at_ms,
    })
}

impl ReleaseStore for PostgresConfigReleases {
    async fn create(&self, release: &NewRelease) -> Result<(), ReleaseStoreError> {
        let tenant = release.tenant_id.to_string();
        // The moment splits back into the columns it is stored as. A draft with no moment writes
        // three nulls, which is what "not timed yet" looks like on the way back in too.
        let (wall_clock_date, wall_clock_time, instant_at_ms) = match &release.moment {
            Some(ReleaseMoment::WallClock { date, time }) => {
                (Some(date.clone()), Some(time.clone()), None)
            }
            Some(ReleaseMoment::Instant(at_ms)) => (None, None, Some(*at_ms)),
            None => (None, None, None),
        };
        let row = NewReleaseRow {
            id: &release.id,
            tenant_id: &tenant,
            name: &release.name,
            // Every release starts as a draft, whatever it was given: nothing fires until an
            // operator says so (ADR-0125 §4).
            status: ReleaseStatus::Draft.as_wire(),
            target_group_id: release.target_group_id.as_deref(),
            wall_clock_date: wall_clock_date.as_deref(),
            wall_clock_time: wall_clock_time.as_deref(),
            instant_at_ms,
            created_by: &release.created_by,
        };
        self.create(&row)
            .await
            .map_err(|error| ReleaseStoreError::new(error.to_string()))
    }

    async fn list_for_tenant(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Release>, ReleaseStoreError> {
        let rows = self
            .list_for_tenant(&tenant_id.to_string())
            .await
            .map_err(|error| ReleaseStoreError::new(error.to_string()))?;
        rows.into_iter().map(release_from_row).collect()
    }

    async fn fetch_one(
        &self,
        tenant_id: TenantId,
        id: &str,
    ) -> Result<Option<Release>, ReleaseStoreError> {
        let row = self
            .fetch_one(&tenant_id.to_string(), id)
            .await
            .map_err(|error| ReleaseStoreError::new(error.to_string()))?;
        row.map(release_from_row).transpose()
    }

    async fn set_status(
        &self,
        tenant_id: TenantId,
        id: &str,
        status: ReleaseStatus,
    ) -> Result<bool, ReleaseStoreError> {
        self.set_status(&tenant_id.to_string(), id, status.as_wire())
            .await
            .map_err(|error| ReleaseStoreError::new(error.to_string()))
    }

    async fn in_flight(&self) -> Result<Vec<Release>, ReleaseStoreError> {
        let rows = self
            .in_flight()
            .await
            .map_err(|error| ReleaseStoreError::new(error.to_string()))?;
        rows.into_iter().map(release_from_row).collect()
    }
}

// --- The console audit trail (ADR-0069) -------------------------------------------------------

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
    Ok(Versioned::new(
        Employee {
            employee_id,
            tenant_id,
            code: row.code,
            name: row.name,
            status: EntityStatus::from_db(&row.status),
            has_pin: row.has_pin,
        },
        version,
    ))
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
        .map_err(|error| {
            // The adapter reports the unique index refusing a second code as `already_exists`: a
            // conflict the operator resolves by choosing another code, not an outage.
            if error.status() == ErrorStatus::AlreadyExists {
                EmployeeStoreError::CodeInUse
            } else {
                EmployeeStoreError::new(error.to_string())
            }
        })
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
/// the two `jsonb` permission arrays.
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
    let permissions_with_approval: Vec<String> =
        serde_json::from_str(&row.permissions_with_approval_json).map_err(|error| {
            RoleTemplateStoreError::new(format!(
                "stored role-template permissions with approval are not JSON: {error}"
            ))
        })?;
    let version = Version::new(row.version);
    Ok(Versioned::new(
        RoleTemplate {
            role_template_id,
            tenant_id,
            name: row.name,
            permissions,
            permissions_with_approval,
            discount_ceiling_minor: row.discount_ceiling_minor,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

/// A list of permission ids as the JSON array text a role's `jsonb` columns take.
fn permission_ids_json(ids: &[String]) -> Result<String, RoleTemplateStoreError> {
    serde_json::to_string(ids).map_err(|error| {
        RoleTemplateStoreError::new(format!("cannot serialize permissions: {error}"))
    })
}

impl RoleTemplateStore for PostgresPeople {
    async fn create(&self, template: &NewRoleTemplate) -> Result<Version, RoleTemplateStoreError> {
        let permissions_json = permission_ids_json(&template.permissions)?;
        let permissions_with_approval_json =
            permission_ids_json(&template.permissions_with_approval)?;
        self.insert_role_template(
            &template.role_template_id.to_string(),
            &template.tenant_id.to_string(),
            &template.name,
            &permissions_json,
            &permissions_with_approval_json,
            template.discount_ceiling_minor,
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
        let permissions_json = permission_ids_json(&template.permissions)?;
        let permissions_with_approval_json =
            permission_ids_json(&template.permissions_with_approval)?;
        self.set_role_template(
            &template.tenant_id.to_string(),
            &template.role_template_id.to_string(),
            &template.name,
            &permissions_json,
            &permissions_with_approval_json,
            template.status.as_str(),
            template.discount_ceiling_minor,
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| RoleTemplateStoreError::new(error.to_string()))
    }
}

/// Parses one stored assignment id, naming the field in the error for the server log.
fn parse_assignment_ulid(text: &str, what: &str) -> Result<Ulid, AssignmentStoreError> {
    text.parse::<Ulid>().map_err(|error| {
        AssignmentStoreError::new(format!("stored {what} id is not a ULID: {error}"))
    })
}

/// The scope a stored row names: `STORE` with its store, `STORE_GROUP` with its group, or `TENANT`
/// with neither (`employee_scope_assignments`, migration 0077).
fn stored_scope(row: &AssignmentRow) -> Result<AssignmentScope, AssignmentStoreError> {
    match (
        row.scope_kind.as_str(),
        row.store_id.as_deref(),
        row.store_group_id.as_deref(),
    ) {
        ("STORE", Some(store_id), None) => Ok(AssignmentScope::Store(StoreId::new(
            parse_assignment_ulid(store_id, "store")?,
        ))),
        ("STORE_GROUP", None, Some(store_group_id)) => Ok(AssignmentScope::StoreGroup(
            StoreGroupId::new(parse_assignment_ulid(store_group_id, "store-group")?),
        )),
        ("TENANT", None, None) => Ok(AssignmentScope::Tenant),
        (kind, _, _) => Err(AssignmentStoreError::new(format!(
            "stored assignment scope {kind} does not carry the id it needs"
        ))),
    }
}

/// Converts a stored assignment row into the domain [`Assignment`], parsing its ids and scope.
fn assignment_record(row: &AssignmentRow) -> Result<Assignment, AssignmentStoreError> {
    Ok(Assignment {
        assignment_id: AssignmentId::new(parse_assignment_ulid(&row.id, "assignment")?),
        tenant_id: TenantId::new(parse_assignment_ulid(&row.tenant_id, "tenant")?),
        employee_id: EmployeeId::new(parse_assignment_ulid(&row.employee_id, "employee")?),
        scope: stored_scope(row)?,
        role_template_id: RoleTemplateId::new(parse_assignment_ulid(
            &row.role_template_id,
            "role-template",
        )?),
        employee_name: row.employee_name.clone(),
        employee_code: row.employee_code.clone(),
    })
}

/// Converts a list of stored assignment rows, failing on the first that does not parse.
fn assignment_records(
    rows: Result<Vec<AssignmentRow>, PortError>,
) -> Result<Vec<Assignment>, AssignmentStoreError> {
    rows.map_err(|error| AssignmentStoreError::new(error.to_string()))?
        .iter()
        .map(assignment_record)
        .collect()
}

impl AssignmentStore for PostgresPeople {
    async fn assign(&self, assignment: &NewAssignment) -> Result<(), AssignmentStoreError> {
        let assignment_id = assignment.assignment_id.to_string();
        let tenant_id = assignment.tenant_id.to_string();
        let employee_id = assignment.employee_id.to_string();
        let role_template_id = assignment.role_template_id.to_string();
        // A one-store grant is a row of the table it always was, so a release without the wider
        // scopes still reads it; a wider one is a row of its own table (migration 0077).
        let written = match assignment.scope {
            AssignmentScope::Store(store_id) => {
                self.insert_assignment(
                    &assignment_id,
                    &tenant_id,
                    &employee_id,
                    &store_id.to_string(),
                    &role_template_id,
                )
                .await
            }
            AssignmentScope::StoreGroup(store_group_id) => {
                self.insert_scope_assignment(
                    &assignment_id,
                    &tenant_id,
                    &employee_id,
                    "STORE_GROUP",
                    Some(&store_group_id.to_string()),
                    &role_template_id,
                )
                .await
            }
            AssignmentScope::Tenant => {
                self.insert_scope_assignment(
                    &assignment_id,
                    &tenant_id,
                    &employee_id,
                    "TENANT",
                    None,
                    &role_template_id,
                )
                .await
            }
        };
        // The adapter reports the unique index refusing a second row as `already_exists`: the
        // person is assigned there already, which is a conflict and not an outage.
        written.map_err(|error| {
            if error.status() == ErrorStatus::AlreadyExists {
                AssignmentStoreError::AlreadyAssigned(error.message().to_owned())
            } else {
                AssignmentStoreError::new(error.to_string())
            }
        })
    }

    async fn list_for_store(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        assignment_records(
            self.fetch_assignments_for_store(&tenant.to_string(), &store_id.to_string())
                .await,
        )
    }

    async fn list_for_group(
        &self,
        tenant: TenantId,
        store_group_id: StoreGroupId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        assignment_records(
            self.fetch_assignments_for_group(&tenant.to_string(), &store_group_id.to_string())
                .await,
        )
    }

    async fn list_tenant_wide(
        &self,
        tenant: TenantId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        assignment_records(
            self.fetch_tenant_wide_assignments(&tenant.to_string())
                .await,
        )
    }

    async fn list_for_employee(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        assignment_records(
            self.fetch_assignments_for_employee(&tenant.to_string(), &employee_id.to_string())
                .await,
        )
    }

    async fn list_for_role(
        &self,
        tenant: TenantId,
        role_template_id: RoleTemplateId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        assignment_records(
            self.fetch_assignments_for_role(&tenant.to_string(), &role_template_id.to_string())
                .await,
        )
    }

    async fn get(
        &self,
        tenant: TenantId,
        assignment_id: AssignmentId,
    ) -> Result<Option<Assignment>, AssignmentStoreError> {
        let row = self
            .fetch_assignment(&tenant.to_string(), &assignment_id.to_string())
            .await
            .map_err(|error| AssignmentStoreError::new(error.to_string()))?;
        row.as_ref().map(assignment_record).transpose()
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

/// The OTA release registry over PostgreSQL ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md),
/// roadmap-v3 slice R2).
///
/// The immutability rule is [`admit_artifact`]'s, consulted here with the digest the table already
/// holds — so the registry and the rule cannot drift, and the refusal a re-upload gets is the one the
/// unit tests pin rather than a second copy of it written in SQL.
impl ArtifactStore for PostgresArtifacts {
    async fn record_artifact(
        &self,
        artifact: &ReleaseArtifact,
    ) -> Result<RecordOutcome, ArtifactStoreError> {
        let target = artifact.target.to_string();
        let stored = self
            .stored_digest(&artifact.release, &target)
            .await
            .map_err(|error| ArtifactStoreError::unavailable(error.to_string()))?;
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
        .map_err(|error| ArtifactStoreError::unavailable(error.to_string()))?;
        Ok(outcome)
    }

    async fn find_artifact(
        &self,
        release: &str,
        target: &TargetTriple,
    ) -> Result<Option<ReleaseArtifact>, ArtifactStoreError> {
        // Fully qualified deliberately: the inherent method and this trait method share a name, and
        // resolution silently prefers the inherent one. Spelling it out means renaming the inherent
        // method is a compile error rather than an infinite recursion.
        let row = PostgresArtifacts::find_artifact(self, release, target.as_str())
            .await
            .map_err(|error| ArtifactStoreError::unavailable(error.to_string()))?;
        row.map(release_artifact_from_row).transpose()
    }

    async fn list_artifacts(
        &self,
        release: &str,
    ) -> Result<Vec<ReleaseArtifact>, ArtifactStoreError> {
        // Fully qualified for the same reason, and more sharply: the inherent `list_artifacts` has
        // the *identical* signature to this one, so a bare `self.list_artifacts(release)` would
        // recurse forever the moment the inherent method went away.
        let rows = PostgresArtifacts::list_artifacts(self, release)
            .await
            .map_err(|error| ArtifactStoreError::unavailable(error.to_string()))?;
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
) -> Result<ReleaseArtifact, ArtifactStoreError> {
    let target = TargetTriple::parse(&row.target).map_err(|error| {
        ArtifactStoreError::unavailable(format!("a stored release target is invalid: {error}"))
    })?;
    let recorded_at = Timestamp::from_milliseconds_since_epoch(row.recorded_at).map_err(|_| {
        ArtifactStoreError::unavailable("a stored release recorded_at is out of range")
    })?;
    Ok(ReleaseArtifact {
        release: row.release,
        target,
        size_bytes: row.size_bytes,
        sha256: row.sha256,
        recorded_at,
    })
}

/// Turns a stored group row into the cloud shape, or reports the id that could not be parsed.
///
/// A row whose id is not a ULID is a `503` rather than a silently skipped group: the ids in this
/// table are minted by [`crate::http`] and nothing else writes them, so an unparseable one means the
/// data has been tampered with or the schema has drifted, and either is worse than unavailable.
fn store_group_from_row(
    tenant_id: TenantId,
    row: &StoreGroupRow,
) -> Result<Versioned<StoreGroup>, StoreGroupStoreError> {
    let group_id = parse_group_id(&row.group_id)?;
    // `from_db` rather than a strict parse: an unrecognised value fails safe to *visible*, which
    // for a cohort means an operator can see and fix it rather than losing a group to a typo in the
    // one column a human might ever edit by hand.
    let status = EntityStatus::from_db(&row.status);
    Ok(Versioned::new(
        StoreGroup {
            group_id,
            tenant_id,
            name: row.name.clone(),
            status,
        },
        Version::new(row.version.clone()),
    ))
}

/// Parses a stored group id.
fn parse_group_id(text: &str) -> Result<StoreGroupId, StoreGroupStoreError> {
    text.parse::<Ulid>()
        .map(StoreGroupId::new)
        .map_err(|_ignored| {
            StoreGroupStoreError(format!("a stored group id is not a ULID: {text}"))
        })
}

/// Parses a stored store id.
fn parse_member_id(text: &str) -> Result<StoreId, StoreGroupStoreError> {
    text.parse::<Ulid>().map(StoreId::new).map_err(|_ignored| {
        StoreGroupStoreError(format!("a stored member id is not a ULID: {text}"))
    })
}

/// Maps an adapter failure onto the seam's one error.
fn store_group_unavailable(error: &PortError) -> StoreGroupStoreError {
    StoreGroupStoreError(error.to_string())
}

impl StoreGroupStore for PostgresStoreGroups {
    async fn create_group(&self, group: &StoreGroup) -> Result<Version, StoreGroupStoreError> {
        let inserted = self
            .insert_group(
                &group.tenant_id.to_string(),
                &group.group_id.to_string(),
                &group.name,
                group.status.as_str(),
            )
            .await
            .map_err(|error| store_group_unavailable(&error))?;
        // A collision is impossible in practice — the id is a freshly minted ULID — so an insert
        // that touched nothing means the id was somehow reused, which is a state to report rather
        // than to paper over with the version of whatever is already there.
        inserted
            .map(Version::new)
            .ok_or_else(|| StoreGroupStoreError(format!("group {} already exists", group.group_id)))
    }

    async fn list_groups(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<StoreGroup>>, StoreGroupStoreError> {
        let rows = self
            .fetch_groups(&tenant_id.to_string())
            .await
            .map_err(|error| store_group_unavailable(&error))?;
        rows.iter()
            .map(|row| store_group_from_row(tenant_id, row))
            .collect()
    }

    async fn update_group(
        &self,
        group: &StoreGroup,
        expected: &Version,
    ) -> Result<UpdateOutcome, StoreGroupStoreError> {
        self.update_group_at(
            &group.tenant_id.to_string(),
            &group.group_id.to_string(),
            &group.name,
            group.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| store_group_unavailable(&error))
    }

    async fn list_members(
        &self,
        tenant_id: TenantId,
        group_id: StoreGroupId,
    ) -> Result<Vec<StoreId>, StoreGroupStoreError> {
        let rows = self
            .fetch_members(&tenant_id.to_string(), &group_id.to_string())
            .await
            .map_err(|error| store_group_unavailable(&error))?;
        rows.iter().map(|id| parse_member_id(id)).collect()
    }

    async fn set_members(
        &self,
        tenant_id: TenantId,
        group_id: StoreGroupId,
        members: &[StoreId],
        expected: &Version,
    ) -> Result<UpdateOutcome, StoreGroupStoreError> {
        let ids: Vec<String> = members.iter().map(ToString::to_string).collect();
        self.set_members_at(
            &tenant_id.to_string(),
            &group_id.to_string(),
            &ids,
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| store_group_unavailable(&error))
    }

    async fn start_batch(&self, batch: &ConfigBatch) -> Result<(), StoreGroupStoreError> {
        let arguments = serde_json::to_string(&batch.arguments).map_err(|error| {
            StoreGroupStoreError(format!("could not serialize batch arguments: {error}"))
        })?;
        self.insert_batch(
            &batch.tenant_id.to_string(),
            &batch.batch_id.to_string(),
            &batch.group_id.to_string(),
            &batch.node,
            &arguments,
            &batch.actor_email,
            batch.started_at_ms,
        )
        .await
        .map_err(|error| store_group_unavailable(&error))
    }

    async fn record_result(
        &self,
        tenant_id: TenantId,
        batch_id: ConfigBatchId,
        result: &BatchResult,
    ) -> Result<(), StoreGroupStoreError> {
        self.upsert_result(
            &tenant_id.to_string(),
            &batch_id.to_string(),
            &result.store_id.to_string(),
            result.outcome.as_wire(),
            result.detail.as_deref(),
            result.version_id.as_deref(),
            result.at_ms,
        )
        .await
        .map_err(|error| store_group_unavailable(&error))
    }

    async fn finish_batch(
        &self,
        tenant_id: TenantId,
        batch_id: ConfigBatchId,
        finished_at_ms: i64,
    ) -> Result<(), StoreGroupStoreError> {
        PostgresStoreGroups::finish_batch(
            self,
            &tenant_id.to_string(),
            &batch_id.to_string(),
            finished_at_ms,
        )
        .await
        .map_err(|error| store_group_unavailable(&error))
    }

    async fn list_batches(
        &self,
        tenant_id: TenantId,
        group_id: StoreGroupId,
        limit: u32,
    ) -> Result<Vec<ConfigBatch>, StoreGroupStoreError> {
        let rows = self
            .fetch_batches(
                &tenant_id.to_string(),
                &group_id.to_string(),
                i64::from(limit),
            )
            .await
            .map_err(|error| store_group_unavailable(&error))?;
        rows.iter()
            .map(|row| {
                Ok(ConfigBatch {
                    batch_id: row
                        .batch_id
                        .parse::<Ulid>()
                        .map(ConfigBatchId::new)
                        .map_err(|_ignored| {
                            StoreGroupStoreError(format!(
                                "a stored batch id is not a ULID: {}",
                                row.batch_id
                            ))
                        })?,
                    tenant_id,
                    group_id: parse_group_id(&row.group_id)?,
                    node: row.node.clone(),
                    arguments: serde_json::from_str(&row.arguments_json).map_err(|error| {
                        StoreGroupStoreError(format!(
                            "stored batch arguments could not be decoded: {error}"
                        ))
                    })?,
                    actor_email: row.actor_email.clone(),
                    started_at_ms: row.started_at_ms,
                    finished_at_ms: row.finished_at_ms,
                })
            })
            .collect()
    }

    async fn batch_results(
        &self,
        tenant_id: TenantId,
        batch_id: ConfigBatchId,
    ) -> Result<Vec<BatchResult>, StoreGroupStoreError> {
        let rows = self
            .fetch_results(&tenant_id.to_string(), &batch_id.to_string())
            .await
            .map_err(|error| store_group_unavailable(&error))?;
        rows.iter()
            .map(|row| {
                Ok(BatchResult {
                    store_id: parse_member_id(&row.store_id)?,
                    outcome: BatchOutcome::from_wire(&row.outcome).ok_or_else(|| {
                        StoreGroupStoreError(format!(
                            "a stored batch result has an unknown outcome: {}",
                            row.outcome
                        ))
                    })?,
                    detail: row.detail.clone(),
                    version_id: row.version_id.clone(),
                    at_ms: row.at_ms,
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Store archives ([ADR-0124](../../../docs/adr/0124-a-store-that-can-be-restored.md))
// ---------------------------------------------------------------------------------------------

/// The store-archive registry over PostgreSQL.
///
/// Thinner than most seams here, because most of what a seam usually does — turn a stored string
/// into a domain type — must **not** happen for the key. It crosses as the wrapped text it is
/// stored as, and only `crate::archive`, which holds the box's `ArchiveSecret`, turns it into a
/// key. Keeping the unwrap out of this file is what makes "the database never sees a usable key" a
/// property of the layering (ADR-0124 Amendment 1).
impl ArchiveStore for PostgresArchives {
    async fn wrapped_key(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> Result<Option<String>, ArchiveStoreError> {
        // Fully qualified: the inherent method and this trait method share a name and signature,
        // and resolution silently prefers the inherent one — so a bare call would recurse forever
        // the day the inherent method is renamed.
        PostgresArchives::wrapped_key(self, &tenant.to_string(), &store.to_string())
            .await
            .map_err(|error| archive_unavailable(&error))
    }

    async fn adopt_key(
        &self,
        tenant: TenantId,
        store: StoreId,
        wrapped: &str,
        minted_at: i64,
    ) -> Result<String, ArchiveStoreError> {
        PostgresArchives::adopt_key(
            self,
            &tenant.to_string(),
            &store.to_string(),
            wrapped,
            minted_at,
        )
        .await
        .map_err(|error| archive_unavailable(&error))
    }

    async fn record_archive(
        &self,
        tenant: TenantId,
        archive: &StoreArchive,
    ) -> Result<(), ArchiveStoreError> {
        PostgresArchives::record_archive(
            self,
            &tenant.to_string(),
            &StoreArchiveRow {
                store_id: archive.store_id.to_string(),
                taken_at: archive.taken_at,
                object_key: archive.object_key.clone(),
                size_bytes: archive.size_bytes,
                sha256: archive.sha256.clone(),
                received_at: archive.received_at,
            },
        )
        .await
        .map_err(|error| archive_unavailable(&error))
    }

    async fn list_archives(
        &self,
        tenant: TenantId,
        store: StoreId,
        limit: i64,
    ) -> Result<Vec<StoreArchive>, ArchiveStoreError> {
        let rows =
            PostgresArchives::list_archives(self, &tenant.to_string(), &store.to_string(), limit)
                .await
                .map_err(|error| archive_unavailable(&error))?;
        rows.into_iter().map(store_archive_from_row).collect()
    }

    async fn expired_before(
        &self,
        cutoff: i64,
        limit: i64,
    ) -> Result<Vec<ExpiredArchive>, ArchiveStoreError> {
        let rows = PostgresArchives::fetch_expired(self, cutoff, limit)
            .await
            .map_err(|error| archive_unavailable(&error))?;
        rows.into_iter().map(expired_archive_from_row).collect()
    }

    async fn forget_archive(
        &self,
        tenant: TenantId,
        store: StoreId,
        taken_at: i64,
    ) -> Result<(), ArchiveStoreError> {
        PostgresArchives::forget_archive(self, &tenant.to_string(), &store.to_string(), taken_at)
            .await
            .map(|_rows| ())
            .map_err(|error| archive_unavailable(&error))
    }
}

/// Rehydrates an expired row, on the same terms as [`store_archive_from_row`].
///
/// An unparseable tenant or store is reported rather than skipped, and here that matters more than
/// it does on the console read: silently skipping would leave a row the sweep can never delete,
/// which is an object nobody can find and nobody stops paying for.
fn expired_archive_from_row(row: ExpiredArchiveRow) -> Result<ExpiredArchive, ArchiveStoreError> {
    let tenant = row
        .tenant_id
        .parse::<Ulid>()
        .map(TenantId::new)
        .map_err(|_| {
            ArchiveStoreError::Unavailable(format!(
                "an expired archive names a tenant id that is not a ULID: {}",
                row.tenant_id
            ))
        })?;
    let store_id = row
        .store_id
        .parse::<Ulid>()
        .map(StoreId::new)
        .map_err(|_| {
            ArchiveStoreError::Unavailable(format!(
                "an expired archive names a store id that is not a ULID: {}",
                row.store_id
            ))
        })?;
    Ok(ExpiredArchive {
        tenant,
        store_id,
        taken_at: row.taken_at,
        object_key: row.object_key,
    })
}

/// Rehydrates a stored row.
///
/// A `store_id` that no longer parses means the row was written by something that did not come
/// through this seam, and it is reported rather than skipped: an archive the cloud cannot describe
/// must not look like an archive that was never taken.
fn store_archive_from_row(row: StoreArchiveRow) -> Result<StoreArchive, ArchiveStoreError> {
    let store_id = row
        .store_id
        .parse::<Ulid>()
        .map(StoreId::new)
        .map_err(|_| {
            ArchiveStoreError::Unavailable(format!(
                "a stored archive names a store id that is not a ULID: {}",
                row.store_id
            ))
        })?;
    Ok(StoreArchive {
        store_id,
        taken_at: row.taken_at,
        object_key: row.object_key,
        size_bytes: row.size_bytes,
        sha256: row.sha256,
        received_at: row.received_at,
    })
}

fn archive_unavailable(error: &PortError) -> ArchiveStoreError {
    ArchiveStoreError::Unavailable(error.to_string())
}

// --- The anchor ledger (ADR-0131 decision 4) -----------------------------------------------------

/// The id a contradiction is filed under: a digest of the finding, not a fresh ULID.
///
/// Derived rather than minted so that re-delivering the same bad anchor — which at-least-once ingest
/// and reconciliation's re-push both guarantee will happen — lands on the row already there instead
/// of adding another. The inputs are everything that makes one finding distinct from another: whose
/// log, at what length, what was held, and what was offered against it. Two separate attempts to
/// rewrite the same point in a store's history differ in `offered_head` and so stay two rows.
fn conflict_id(tenant: TenantId, report: &ConflictReport) -> String {
    let mut digest = Sha256::new();
    for part in [
        tenant.to_string(),
        report.store.to_string(),
        report.chain_seq.to_string(),
        report.held_head.as_str().to_owned(),
        report.offered_head.as_str().to_owned(),
    ] {
        digest.update(part.as_bytes());
        // A separator, so `("ab", "cd")` and `("a", "bcd")` cannot collide into one id.
        digest.update(b"\n");
    }
    ChainHash::of(digest.finalize().into()).as_str().to_owned()
}

/// Maps one stored anchor row onto the ledger's view of it.
fn anchor_from_row(store: StoreId, row: &AnchorRow) -> Result<StoreAnchor, AnchorError> {
    Ok(StoreAnchor {
        store,
        chain_seq: u64::try_from(row.chain_seq)
            .map_err(|_| AnchorError::new("a stored anchor chain_seq is negative"))?,
        head: ChainHash::from_hex(&row.chain_head),
        unchained: u64::try_from(row.unchained)
            .map_err(|_| AnchorError::new("a stored anchor unchained count is negative"))?,
        observed_at: Timestamp::from_milliseconds_since_epoch(row.observed_at)
            .map_err(|_| AnchorError::new("a stored anchor observed_at is out of range"))?,
    })
}

impl ChainWindow for PostgresAnchors {
    fn events_in_window<'a>(
        &'a self,
        tenant: TenantId,
        store: StoreId,
        day: &'a BusinessDate,
        days_back: u8,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<EventEnvelope<RawPayload>>, AnchorError>> {
        Box::pin(async move {
            let rows = self
                .events_in_window(
                    &tenant.to_string(),
                    &store.to_string(),
                    &day.to_string(),
                    i32::from(days_back),
                    i64::from(limit),
                )
                .await
                .map_err(|error| AnchorError::new(error.to_string()))?;
            // An envelope that will not deserialise is skipped rather than failing the window: it is
            // a corrupt row, and losing the whole audit to one of them would hand a store exactly the
            // way to stop being audited.
            Ok(rows
                .iter()
                .filter_map(|row| match serde_json::from_str(row) {
                    Ok(envelope) => Some(envelope),
                    Err(error) => {
                        tracing::warn!(
                            store = %store,
                            %error,
                            "a stored envelope did not deserialise; it is left out of this chain window"
                        );
                        None
                    }
                })
                .collect())
        })
    }
}

impl AnchorLedger for PostgresAnchors {
    fn held_for(
        &self,
        tenant: TenantId,
        store: StoreId,
        chain_seq: u64,
    ) -> BoxFuture<'_, Result<HeldAnchors, AnchorError>> {
        Box::pin(async move {
            // A length beyond `i64` is not a chain any store will ever hold, and it is not this
            // layer's business to have an opinion about it: nothing is held there, which is true.
            let Ok(offered_seq) = i64::try_from(chain_seq) else {
                return Ok(HeldAnchors::default());
            };
            let (head, at_offered_length) = self
                .held_for(&tenant.to_string(), &store.to_string(), offered_seq)
                .await
                .map_err(|error| AnchorError::new(error.to_string()))?;
            Ok(HeldAnchors {
                head: head
                    .as_ref()
                    .map(|row| anchor_from_row(store, row))
                    .transpose()?,
                at_offered_length: at_offered_length
                    .as_ref()
                    .map(|row| anchor_from_row(store, row))
                    .transpose()?,
            })
        })
    }

    fn accept<'a>(
        &'a self,
        tenant: TenantId,
        anchor: &'a StoreAnchor,
    ) -> BoxFuture<'a, Result<(), AnchorError>> {
        Box::pin(async move {
            let row = AnchorRow {
                chain_seq: i64::try_from(anchor.chain_seq)
                    .map_err(|_| AnchorError::new("a chain_seq beyond i64 cannot be recorded"))?,
                chain_head: anchor.head.as_str().to_owned(),
                unchained: i64::try_from(anchor.unchained).unwrap_or(i64::MAX),
                observed_at: anchor.observed_at.as_milliseconds_since_epoch(),
            };
            self.record_anchor(&tenant.to_string(), &anchor.store.to_string(), &row)
                .await
                .map_err(|error| AnchorError::new(error.to_string()))
        })
    }

    fn record_conflict<'a>(
        &'a self,
        tenant: TenantId,
        report: &'a ConflictReport,
    ) -> BoxFuture<'a, Result<(), AnchorError>> {
        Box::pin(async move {
            let row = ConflictRow {
                chain_seq: i64::try_from(report.chain_seq).unwrap_or(i64::MAX),
                held_head: report.held_head.as_str().to_owned(),
                offered_head: report.offered_head.as_str().to_owned(),
                offered_event_id: report.offered_event_id.to_string(),
            };
            self.record_conflict(
                &conflict_id(tenant, report),
                &tenant.to_string(),
                &report.store.to_string(),
                &row,
            )
            .await
            .map_err(|error| AnchorError::new(error.to_string()))
        })
    }

    fn list_conflicts(
        &self,
        tenant: TenantId,
        store: Option<StoreId>,
        limit: u32,
    ) -> BoxFuture<'_, Result<Vec<AnchorConflict>, AnchorError>> {
        Box::pin(async move {
            let store_string = store.map(|id| id.to_string());
            let rows = self
                .list_conflicts(
                    &tenant.to_string(),
                    store_string.as_deref(),
                    i64::from(limit),
                )
                .await
                .map_err(|error| AnchorError::new(error.to_string()))?;
            rows.into_iter()
                .map(|row| {
                    let store = row
                        .store_id
                        .parse::<Ulid>()
                        .map(StoreId::new)
                        .map_err(|_| {
                            AnchorError::new("a stored conflict store_id is not a ULID")
                        })?;
                    let offered_event_id = row
                        .conflict
                        .offered_event_id
                        .parse::<Ulid>()
                        .map(EventId::new)
                        .map_err(|_| {
                            AnchorError::new("a stored conflict offered_event_id is not a ULID")
                        })?;
                    Ok(AnchorConflict {
                        conflict_id: row.conflict_id,
                        noticed_at: Timestamp::from_milliseconds_since_epoch(row.noticed_at)
                            .map_err(|_| {
                                AnchorError::new("a stored conflict noticed_at is out of range")
                            })?,
                        report: ConflictReport {
                            store,
                            chain_seq: u64::try_from(row.conflict.chain_seq).map_err(|_| {
                                AnchorError::new("a stored conflict chain_seq is negative")
                            })?,
                            held_head: ChainHash::from_hex(&row.conflict.held_head),
                            offered_head: ChainHash::from_hex(&row.conflict.offered_head),
                            offered_event_id,
                        },
                    })
                })
                .collect()
        })
    }
}
