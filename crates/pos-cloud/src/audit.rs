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
use core::pin::Pin;

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

/// A filter for the audit read ([`AuditStore::query`]). Every field is optional; a `None` matches
/// everything, so an all-`None` query with a `limit` is the plain newest-first read. `tenant`
/// scoping is special: `Some(tenant)` returns only that tenant's rows (never the tenant-global
/// `None`-tenant rows), while `None` reads across every tenant including the global ones — the
/// trusted fleet-wide read the console's Audit screen uses.
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    /// The tenant to scope to, or `None` for the fleet-wide read.
    pub tenant: Option<TenantId>,
    /// Only entries for this entity type (e.g. `store`), or `None` for any.
    pub entity_type: Option<String>,
    /// Only entries for this entity id, or `None` for any.
    pub entity_id: Option<String>,
    /// Only entries with this action (e.g. `store.update`), or `None` for any.
    pub action: Option<String>,
    /// Only entries by this acting admin id, or `None` for any.
    pub actor_admin_id: Option<String>,
    /// Only entries at or after this instant (Unix ms), or `None` for no lower bound.
    pub since_ms: Option<i64>,
    /// Only entries at or before this instant (Unix ms), or `None` for no upper bound.
    pub until_ms: Option<i64>,
    /// The most rows to return, newest first.
    pub limit: u32,
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

    /// Reads entries newest-first matching every set filter of `query` (a `None` filter matches
    /// everything), up to `query.limit`. The filters are applied before the limit, so a narrow filter
    /// still reaches older matching rows. This is what the Audit screen reads.
    ///
    /// # Errors
    ///
    /// [`AuditStoreError`] if the entries could not be read.
    fn query(
        &self,
        query: &AuditQuery,
    ) -> impl Future<Output = Result<Vec<AuditEntry>, AuditStoreError>> + Send;
}

/// An object-safe audit recorder: records one entry, best-effort. This is what the HTTP routes carry
/// (as `Arc<dyn AuditRecorder>`), so every write handler can emit an audit entry without threading an
/// `AuditStore` generic through the router's already-large type parameters. The append future is boxed
/// so the trait stays object-safe; a store failure is the recorder's to log and swallow, never the
/// caller's to propagate — a mutation that succeeded must not fail because its audit write did
/// ([ADR-0069](../../../docs/adr/0069-audit-trail.md)).
pub trait AuditRecorder: Send + Sync {
    /// Records `entry`, awaiting the underlying append; a failure is logged, not returned.
    fn record(&self, entry: AuditEntry) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// Wraps a concrete [`AuditStore`] as an object-safe [`AuditRecorder`], logging (never propagating) an
/// append failure. The binary wraps its `store-postgres` audit store in this and hands the router an
/// `Arc<dyn AuditRecorder>`.
#[derive(Debug, Clone)]
pub struct AuditSink<Au> {
    store: Au,
}

impl<Au> AuditSink<Au> {
    /// Wraps `store`.
    pub const fn new(store: Au) -> Self {
        Self { store }
    }
}

impl<Au> AuditRecorder for AuditSink<Au>
where
    Au: AuditStore + Send + Sync,
{
    fn record(&self, entry: AuditEntry) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if let Err(error) = self.store.append(&entry).await {
                tracing::error!(
                    %error,
                    action = %entry.action,
                    entity_type = %entry.entity_type,
                    "recording a console audit entry failed"
                );
            }
        })
    }
}

/// An audit recorder that drops every entry — the default a router carries when no audit store is
/// wired (tests that do not assert on audit), so a handler can always call `record` unconditionally.
#[derive(Debug, Clone, Copy)]
pub struct NoopAuditRecorder;

impl AuditRecorder for NoopAuditRecorder {
    fn record(&self, _entry: AuditEntry) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
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
