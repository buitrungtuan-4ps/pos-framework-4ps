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

use pos_proto::devices::DeviceConnection;
use pos_proto::ids::{StationId, StoreId, TenantId};
use pos_proto::ulid::Ulid;

/// Which kind of device a proposal is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// An ESC/POS receipt or kitchen printer.
    Printer,
    /// A kitchen-display station.
    Kds,
    /// A fixed POS terminal that may hold a printer's transport
    /// ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
    ///
    /// The one kind a store cannot *propose*, because nothing on a LAN announces itself as a till.
    /// An operator creates it in the console — see [`DeviceProposalStore::create_terminal`] — and
    /// the named admin who did so is the human gate ADR-0041's approval step exists to be.
    Terminal,
}

impl DeviceKind {
    /// The `snake_case` wire name.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Printer => "printer",
            Self::Kds => "kds",
            Self::Terminal => "terminal",
        }
    }

    /// Parses a wire name, or `None` for one this build does not know.
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        match name {
            "printer" => Some(Self::Printer),
            "kds" => Some(Self::Kds),
            "terminal" => Some(Self::Terminal),
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
    /// How the device is attached (`usb`/`network`/`serial`), as approval recorded it — `None` while
    /// pending, because discovery cannot find this out ([ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md)).
    pub connection: Option<String>,
    /// The kitchen station this device serves, as approval recorded it. `None` for the counter's
    /// receipt printer, which serves the bill rather than a station — and `None` while pending.
    pub station_id: Option<String>,
    /// The `terminal` device whose transport reaches this printer, if an operator has picked one
    /// ([ADR-0112](../../../docs/adr/0112-print-agents.md)). `None` — the ordinary case — means the
    /// edge opens the address itself, exactly as it always has.
    pub agent_device_id: Option<String>,
    /// `pending`, `approved`, or `rejected`.
    pub status: String,
    /// The version the row was read at, for a conditional write
    /// ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)). Opaque: the adapter
    /// mints it from `xmin::text`, and nothing above this seam may assume that.
    pub version: String,
}

/// What a conditional agent write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetAgentOutcome {
    /// The row was changed, and now sits at this version.
    Updated,
    /// No **approved** row with that id exists in this tenant.
    NotFound,
    /// The row exists, but has moved on from the version the caller read it at.
    VersionMismatch,
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
    /// An approval also records the two facts discovery cannot find and printing cannot do without
    /// ([ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md)): `connection`, because a
    /// cash drawer opens only over USB, and `station`, because a fired line has to route somewhere.
    /// `station` is `None` for the counter's receipt printer, which serves the bill rather than a
    /// station. A rejection passes `None` for both — they describe a device the store will address,
    /// and a rejected one never will.
    ///
    /// # Errors
    ///
    /// [`DeviceProposalError`] if the store could not be written.
    fn resolve(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        approved: bool,
        connection: Option<DeviceConnection>,
        station: Option<StationId>,
    ) -> impl Future<Output = Result<bool, DeviceProposalError>> + Send;

    /// Writes an **already-approved** `terminal` row directly, skipping propose→approve
    /// ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
    ///
    /// Not a hole in ADR-0041's human gate — it *is* that gate, moved to the only place it can be
    /// for this kind. Discovery exists because a port-9100 printer answers an mDNS query and must
    /// not become live on that basis; a human deciding is what turns a discovery into a device.
    /// Nothing announces itself as a POS terminal, so there is no discovery to gate: the operator
    /// knows the machine exists because somebody carried it into the shop. The console write is
    /// therefore the decision, made by a named admin holding `ManageDevices` and audited as such,
    /// and the proposal path keeps its two kinds and its meaning rather than being stretched to
    /// cover a device no store can propose.
    ///
    /// A terminal carries no `connection` and no `address`: nothing dials it, and the agent connects
    /// outbound to the edge.
    ///
    /// # Errors
    ///
    /// [`DeviceProposalError`] if the store could not be written.
    fn create_terminal(
        &self,
        proposal: &PersistedDeviceProposal,
    ) -> impl Future<Output = Result<(), DeviceProposalError>> + Send;

    /// Points an approved device at the terminal whose transport reaches it, or clears the pointer
    /// with `None` ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
    ///
    /// Conditional on `expected`
    /// ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)): two managers picking
    /// different agents for one printer is the ordinary race here, and last-write-wins would leave
    /// the loser believing a decision that is not in the database. Scoped to `tenant`, and to rows
    /// that are **approved** — a pending proposal has no place to print from yet.
    ///
    /// This seam does not check that `agent` names a terminal. That check belongs where the whole
    /// node is visible at once, because a reference is only resolvable against the set being
    /// published; see `admin_publish_devices`.
    ///
    /// # Errors
    ///
    /// [`DeviceProposalError`] if the store could not be written.
    fn set_agent(
        &self,
        tenant: TenantId,
        id: DeviceProposalId,
        agent: Option<DeviceProposalId>,
        expected: &str,
    ) -> impl Future<Output = Result<SetAgentOutcome, DeviceProposalError>> + Send;
}
