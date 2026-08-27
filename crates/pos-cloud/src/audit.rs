// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The console audit-trail seam ([ADR-0069](../../../docs/adr/0069-audit-trail.md), Track G2).
//!
//! An append-only record of every console mutation: who did it (a snapshot of the acting admin, so
//! renaming or deleting the admin later never rewrites history), what they did, to which entity, and
//! the before/after of the change. The `/admin` write routes append one entry per successful mutation
//! (later slices); this slice defines the seam, the entry shape, and a recent-first `list` the audit
//! screen reads.
//!
//! The append is **best-effort after the write**, not inside the mutation's transaction — the `/admin`
//! routes use per-seam adapters with no shared transaction, so a caller logs an append failure loudly
//! rather than failing the mutation the operator asked for (ADR-0069). A trait so it runs against an
//! in-memory fake in tests and the `audit_log` table in the cloud (the impl lives in
//! [`crate::persistence`], the SQL in `store-postgres`).

use core::fmt;
use core::future::Future;

use pos_proto::ids::TenantId;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;

use crate::auth::admin::AdminRole;

/// An audit entry's own identifier — a ULID minted at the edge when the entry is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuditId(Ulid);

impl AuditId {
    /// Wraps a ULID as an audit id.
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

impl fmt::Display for AuditId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// The acting admin as snapshotted onto an audit entry — id, email, and role copied at action time,
/// so the entry stays truthful even after the admin is renamed, re-roled, or removed.
#[derive(Debug, Clone)]
pub struct AuditActor {
    /// The admin's ULID id.
    pub admin_id: String,
    /// The admin's login email at action time.
    pub email: String,
    /// The admin's role at action time.
    pub role: AdminRole,
}

/// One recorded console mutation.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    /// The entry's own id.
    pub id: AuditId,
    /// The tenant the action belongs to, or `None` for a tenant-global action (a tenant create, admin
    /// management, the break-glass reset).
    pub tenant_id: Option<TenantId>,
    /// Who did it.
    pub actor: AuditActor,
    /// The action, `resource.verb` (e.g. `store.update`).
    pub action: String,
    /// The affected entity's type (e.g. `store`).
    pub entity_type: String,
    /// The affected entity's id.
    pub entity_id: String,
    /// The prior value, or `None` for a create.
    pub before: Option<serde_json::Value>,
    /// The new value, or `None` for a delete.
    pub after: Option<serde_json::Value>,
    /// A request-correlation id, or `None` until request-id plumbing lands.
    pub request_id: Option<String>,
    /// When the action happened.
    pub at: Timestamp,
}

/// Appends and reads console audit entries.
pub trait AuditStore {
    /// Appends one entry. Append-only: there is no update or delete.
    ///
    /// # Errors
    ///
    /// [`AuditStoreError`] if the entry could not be written.
    fn append(
        &self,
        entry: &AuditEntry,
    ) -> impl Future<Output = Result<(), AuditStoreError>> + Send;

    /// Lists entries newest-first, up to `limit`. `Some(tenant)` scopes to one tenant; `None` reads
    /// across every tenant (the trusted fleet-wide read, including tenant-global entries).
    ///
    /// # Errors
    ///
    /// [`AuditStoreError`] if the entries could not be read.
    fn list(
        &self,
        tenant: Option<TenantId>,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<AuditEntry>, AuditStoreError>> + Send;
}

/// A failure of the audit store itself — the database is unreachable, or a stored row could not be
/// decoded.
#[derive(Debug, thiserror::Error)]
#[error("the audit store failed: {0}")]
pub struct AuditStoreError(String);

impl AuditStoreError {
    /// Wraps a message (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}
