// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Wiring the device seams to their `store-postgres` tables: the `ClaimStore`
//! ([ADR-0148](../../../docs/adr/0148-an-unclaimed-box-shows-a-code-and-the-console-claims-it.md)),
//! the `ActivationCodeStore` ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)) and
//! the `DeviceProposalStore` ([ADR-0041](../../../docs/adr/0041-device-onboarding.md)).
//!
//! Moved out of `pos-cloud`'s `persistence.rs` with the traits (ADR-0170 decision 4): a trait can
//! be implemented for another crate's type only in the trait's own crate. The SQL stays in
//! `store-postgres`.

use pos_core::activation::CodeStatus;
use pos_proto::devices::{DeviceConnection, PaperWidth};
use pos_proto::ids::{DeviceId, StationId, StoreId, TenantId};
use pos_proto::printing::{ReceiptLanguage, ReceiptSecondLanguage};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use store_postgres::{
    ClaimBindRow, ClaimCollectRow, ClaimSlotRow, PostgresActivationCodes, PostgresClaims,
    PostgresDeviceProposals,
};

use crate::activation::{ActivationCodeStore, ActivationStoreError, DeviceCredential, IssuedCode};
use crate::claim::{BindOutcome, ClaimStore, ClaimStoreError, CollectOutcome};
use crate::devices::{
    DeviceProposalError, DeviceProposalId, DeviceProposalStatus, DeviceProposalStore,
    DeviceProposalSummary, DeviceWriteOutcome, PersistedDeviceProposal,
};

impl ClaimStore for PostgresClaims {
    async fn open(
        &self,
        claim_id: Ulid,
        user_code_hash: [u8; 32],
        secret_hash: [u8; 32],
        expires_at: Timestamp,
    ) -> Result<(), ClaimStoreError> {
        self.open(
            &claim_id.to_string(),
            &user_code_hash,
            &secret_hash,
            expires_at.as_milliseconds_since_epoch(),
        )
        .await
        .map_err(|error| ClaimStoreError::new(error.to_string()))
    }

    async fn bind(
        &self,
        user_code_hash: [u8; 32],
        tenant_id: TenantId,
        store_id: StoreId,
        device_id: DeviceId,
        now: Timestamp,
    ) -> Result<BindOutcome, ClaimStoreError> {
        let slot = ClaimSlotRow {
            tenant_id: tenant_id.to_string(),
            store_id: store_id.to_string(),
            device_id: device_id.to_string(),
        };
        let outcome = self
            .bind(&user_code_hash, &slot, now.as_milliseconds_since_epoch())
            .await
            .map_err(|error| ClaimStoreError::new(error.to_string()))?;
        Ok(match outcome {
            ClaimBindRow::Bound => BindOutcome::Bound,
            ClaimBindRow::Unknown => BindOutcome::Unknown,
            ClaimBindRow::Expired => BindOutcome::Expired,
            ClaimBindRow::AlreadyBound => BindOutcome::AlreadyBound,
        })
    }

    async fn collect(
        &self,
        claim_id: Ulid,
        secret_hash: [u8; 32],
        credential: &DeviceCredential,
        now: Timestamp,
    ) -> Result<CollectOutcome, ClaimStoreError> {
        let outcome = self
            .collect(
                &claim_id.to_string(),
                &secret_hash,
                &credential.id.to_string(),
                &credential.secret_hash(),
                now.as_milliseconds_since_epoch(),
            )
            .await
            .map_err(|error| ClaimStoreError::new(error.to_string()))?;
        Ok(match outcome {
            ClaimCollectRow::Collected(slot) => {
                let parse = |what: &str, text: &str| {
                    text.parse::<Ulid>().map_err(|_| {
                        ClaimStoreError::new(format!("claim {claim_id}: {what} is not a ULID"))
                    })
                };
                CollectOutcome::Collected {
                    tenant_id: TenantId::new(parse("tenant id", &slot.tenant_id)?),
                    store_id: StoreId::new(parse("store id", &slot.store_id)?),
                    device_id: DeviceId::new(parse("device id", &slot.device_id)?),
                }
            }
            ClaimCollectRow::Pending => CollectOutcome::Pending,
            ClaimCollectRow::Expired => CollectOutcome::Expired,
            ClaimCollectRow::Refused => CollectOutcome::Refused,
        })
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
                agent_device_id: row.agent_device_id,
                drawer_attached: row.drawer_attached,
                paper_width: row.paper_width,
                cuts_paper: row.cuts_paper,
                receipt_printer_id: row.receipt_printer_id,
                receipt_language: row.receipt_language,
                receipt_second_language: row.receipt_second_language,
                opening_float_minor: row.opening_float_minor,
                status: row.status,
                version: row.version,
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
            connection,
            station.as_deref(),
        )
        .await
        .map_err(|error| DeviceProposalError::new(error.to_string()))
    }

    async fn create_terminal(
        &self,
        proposal: &PersistedDeviceProposal,
    ) -> Result<(), DeviceProposalError> {
        PostgresDeviceProposals::create_terminal(
            self,
            &proposal.id.to_string(),
            &proposal.tenant_id.to_string(),
            &proposal.store_id.to_string(),
            &proposal.name,
        )
        .await
        .map_err(|error| DeviceProposalError::new(error.to_string()))
    }

    async fn set_agent(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        agent: Option<DeviceProposalId>,
        expected: &str,
    ) -> Result<DeviceWriteOutcome, DeviceProposalError> {
        let tenant = tenant.to_string();
        let id = id.to_string();
        let agent = agent.map(|agent| agent.to_string());
        let changed =
            PostgresDeviceProposals::set_agent(self, &tenant, &id, agent.as_deref(), expected)
                .await
                .map_err(|error| DeviceProposalError::new(error.to_string()))?;
        device_write_outcome(self, &tenant, &id, changed).await
    }

    async fn set_drawer(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        drawer_attached: bool,
        expected: &str,
    ) -> Result<DeviceWriteOutcome, DeviceProposalError> {
        let tenant = tenant.to_string();
        let id = id.to_string();
        let changed =
            PostgresDeviceProposals::set_drawer(self, &tenant, &id, drawer_attached, expected)
                .await
                .map_err(|error| DeviceProposalError::new(error.to_string()))?;
        device_write_outcome(self, &tenant, &id, changed).await
    }

    async fn set_paper(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        paper_width: PaperWidth,
        cuts_paper: bool,
        expected: &str,
    ) -> Result<DeviceWriteOutcome, DeviceProposalError> {
        let tenant = tenant.to_string();
        let id = id.to_string();
        let changed = PostgresDeviceProposals::set_paper(
            self,
            &tenant,
            &id,
            paper_width,
            cuts_paper,
            expected,
        )
        .await
        .map_err(|error| DeviceProposalError::new(error.to_string()))?;
        device_write_outcome(self, &tenant, &id, changed).await
    }

    async fn set_receipt(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        printer: Option<DeviceProposalId>,
        language: Option<ReceiptLanguage>,
        second_language: Option<ReceiptSecondLanguage>,
        expected: &str,
    ) -> Result<DeviceWriteOutcome, DeviceProposalError> {
        let tenant = tenant.to_string();
        let id = id.to_string();
        let printer = printer.map(|printer| printer.to_string());
        let changed = PostgresDeviceProposals::set_receipt(
            self,
            &tenant,
            &id,
            printer.as_deref(),
            language,
            second_language,
            expected,
        )
        .await
        .map_err(|error| DeviceProposalError::new(error.to_string()))?;
        device_write_outcome(self, &tenant, &id, changed).await
    }

    async fn set_float(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        opening_float_minor: Option<i64>,
        expected: &str,
    ) -> Result<DeviceWriteOutcome, DeviceProposalError> {
        let tenant = tenant.to_string();
        let id = id.to_string();
        let changed =
            PostgresDeviceProposals::set_float(self, &tenant, &id, opening_float_minor, expected)
                .await
                .map_err(|error| DeviceProposalError::new(error.to_string()))?;
        device_write_outcome(self, &tenant, &id, changed).await
    }
}

/// What a conditional write to an approved device did, given the version it `changed` the row to.
///
/// Nothing changed has two reasons, and they need different answers: the caller is stale, or the
/// device is not there at all. A second read is the only way to tell, and it is only ever paid on
/// the failing path.
async fn device_write_outcome(
    devices: &PostgresDeviceProposals,
    tenant: &str,
    id: &str,
    changed: Option<String>,
) -> Result<DeviceWriteOutcome, DeviceProposalError> {
    if changed.is_some() {
        return Ok(DeviceWriteOutcome::Updated);
    }
    let present = devices
        .version_of(tenant, id)
        .await
        .map_err(|error| DeviceProposalError::new(error.to_string()))?;
    Ok(if present.is_some() {
        DeviceWriteOutcome::VersionMismatch
    } else {
        DeviceWriteOutcome::NotFound
    })
}
