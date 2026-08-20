// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The device-onboarding seam ([ADR-0041](../../../docs/adr/0041-device-onboarding.md)).
//!
//! A store discovers a printer or KDS panel on its LAN and *proposes* it; a super-admin *approves* it
//! before it is anything more than a pending suggestion — the human gate that stops an
//! unauthenticated port-9100 device becoming live just because it answered an mDNS query. This seam
//! is the cloud's store of those proposals: `store-postgres` persists a `device_proposals` table, a
//! fake answers from a list, and the routes ([`crate::http`]) drive both.

use core::fmt;
use core::future::Future;

use pos_proto::ids::{StoreId, TenantId};
use pos_proto::ulid::Ulid;

/// Which kind of device a proposal is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// An ESC/POS receipt or kitchen printer.
    Printer,
    /// A kitchen-display station.
    Kds,
}

impl DeviceKind {
    /// The `snake_case` wire name.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Printer => "printer",
            Self::Kds => "kds",
        }
    }

    /// Parses a wire name, or `None` for one this build does not know.
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        match name {
            "printer" => Some(Self::Printer),
            "kds" => Some(Self::Kds),
            _ => None,
        }
    }
}

/// Where a proposal sits in the approval workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceProposalStatus {
    /// Proposed by a store, awaiting an operator's decision.
    Pending,
    /// Approved by a super-admin; the store may use it.
    Approved,
    /// Rejected by a super-admin; not usable.
    Rejected,
}

impl DeviceProposalStatus {
    /// The `snake_case` wire name.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}

/// A device proposal's public identifier — a ULID minted at proposal time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceProposalId(Ulid);

impl DeviceProposalId {
    /// Wraps a ULID as a proposal id.
    #[must_use]
    pub const fn new(ulid: Ulid) -> Self {
        Self(ulid)
    }

    /// The underlying ULID.
    #[must_use]
    pub const fn as_ulid(self) -> Ulid {
        self.0
    }
}

impl fmt::Display for DeviceProposalId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A freshly-proposed device: the discovered facts a store submits. Always stored `pending`.
#[derive(Debug, Clone)]
pub struct PersistedDeviceProposal {
    /// The proposal's id.
    pub id: DeviceProposalId,
    /// The tenant that owns the store.
    pub tenant_id: TenantId,
    /// The store that discovered the device.
    pub store_id: StoreId,
    /// Whether it is a printer or a KDS.
    pub kind: DeviceKind,
    /// A human-readable name for the device.
    pub name: String,
    /// The device's network address, as discovered (e.g. `192.168.1.50:9100`).
    pub address: String,
}

/// A proposal as listed — the durable facts plus its status.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeviceProposalSummary {
    /// The proposal id (a ULID string).
    pub id: String,
    /// The store the device belongs to (a ULID string).
    pub store_id: String,
    /// `printer` or `kds`.
    pub kind: String,
    /// The device's name.
    pub name: String,
    /// The device's network address.
    pub address: String,
    /// `pending`, `approved`, or `rejected`.
    pub status: String,
}

/// A failure of the device-proposal store itself — the database is unreachable.
#[derive(Debug, thiserror::Error)]
#[error("the device-proposal store failed: {0}")]
pub struct DeviceProposalError(String);

impl DeviceProposalError {
    /// A store failure carrying a human-readable reason (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Persists and resolves device proposals.
pub trait DeviceProposalStore {
    /// Stores a freshly-proposed device (status `pending`).
    ///
    /// # Errors
    ///
    /// [`DeviceProposalError`] if the store could not be written.
    fn propose(
        &self,
        proposal: &PersistedDeviceProposal,
    ) -> impl Future<Output = Result<(), DeviceProposalError>> + Send;

    /// Lists a tenant's proposals in a given status — all its stores when `store` is `None` (the admin
    /// pending queue), or one store when `Some` (a store reading its approved devices). Newest first.
    ///
    /// # Errors
    ///
    /// [`DeviceProposalError`] if the store could not be read.
    fn list(
        &self,
        tenant: TenantId,
        store: Option<StoreId>,
        status: DeviceProposalStatus,
    ) -> impl Future<Output = Result<Vec<DeviceProposalSummary>, DeviceProposalError>> + Send;

    /// Transitions a **pending** proposal to approved or rejected, returning whether a pending row was
    /// found and changed — so resolving an already-resolved or unknown id is a no-op, not an error.
    /// Scoped to `tenant`, so one tenant cannot resolve another's proposal.
    ///
    /// # Errors
    ///
    /// [`DeviceProposalError`] if the store could not be written.
    fn resolve(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        approved: bool,
    ) -> impl Future<Output = Result<bool, DeviceProposalError>> + Send;
}
