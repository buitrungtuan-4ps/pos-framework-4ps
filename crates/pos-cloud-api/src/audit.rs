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
use crate::paging::{Page, PageRequest};

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

/// A filter for the audit reads ([`AuditStore::query`] and [`AuditStore::query_page`]). Every field
/// is optional; a `None` matches everything, so an all-`None` filter is the plain newest-first read.
/// `tenant` scoping is special: `Some(tenant)` returns only that tenant's rows (never the
/// tenant-global `None`-tenant rows), while `None` reads across every tenant including the global
/// ones — the trusted fleet-wide read the console's Audit screen uses.
///
/// It carries no `limit`. It used to, which made the paged read ambiguous: handed both a filter with
/// a limit and a [`PageRequest`] with another, an implementation had two answers about the window and
/// no rule for which won. The bound now belongs to whichever read is being asked for — a count for
/// [`query`](AuditStore::query), a page for [`query_page`](AuditStore::query_page) — and this type is
/// only ever about *which rows match* ([ADR-0098](../../../docs/adr/0098-paged-admin-reads.md)).
///
/// It carries no [`TrailOrder`] either, for the same reason and not by oversight: the order is not a
/// filter, and only one of the two reads can honour it. Sitting here it would be a field
/// [`query`](AuditStore::query) had to either ignore or reinterpret — exactly the two-answers shape
/// the `limit` had.
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
}

/// Which end of the trail a paged read starts from.
///
/// Named for the trail rather than for a column. `/admin/catalog/items` spells its direction
/// `asc`/`desc`, but there it is relative to a named `?sort=` field; this route has no sort for a
/// direction to be relative *to*, so "ascending" would have to mean "ascending in time" — the same
/// word doing a different job on two routes, which is worse than two spellings. The trail's two
/// orders have plain names, so it uses them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TrailOrder {
    /// Most recent first — the trail's default, and its only order before this existed.
    #[default]
    Newest,
    /// Earliest first, which is how an incident reads: a `since_ms`/`until_ms` window in the order
    /// the actions actually happened, rather than backwards from the end of it.
    Oldest,
}

impl TrailOrder {
    /// The wire token for this order.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Newest => "newest",
            Self::Oldest => "oldest",
        }
    }

    /// The order a wire token names, or `None` if it names none.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "newest" => Some(Self::Newest),
            "oldest" => Some(Self::Oldest),
            _unknown => None,
        }
    }

    /// Every accepted token, for the refusal that names what a route will take. Kept honest against
    /// the variants by `a_trail_order_token_round_trips_and_an_unknown_one_is_not_read_as_a_default`.
    #[must_use]
    pub const fn tokens() -> &'static [&'static str] {
        &["newest", "oldest"]
    }
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

    /// Reads the newest `limit` entries matching every set filter of `filter` (a `None` filter matches
    /// everything). The filters are applied before the limit, so a narrow filter still reaches older
    /// matching rows.
    ///
    /// A *window*, not a page: this is ADR-0069's read, and `limit` here means "the most recent this
    /// many", which is why `/admin/audit` defaults and clamps it. The per-entity audit panel
    /// (`components/AuditTrail.tsx`) wants exactly this and no count.
    ///
    /// # Errors
    ///
    /// [`AuditStoreError`] if the entries could not be read.
    fn query(
        &self,
        filter: &AuditQuery,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<AuditEntry>, AuditStoreError>> + Send;

    /// One page of the entries matching `filter`, in the order `order` asks for, with how many
    /// matched in total.
    ///
    /// Beside [`query`](Self::query) rather than replacing it, for the reason ADR-0098 gives: the
    /// window and the page are different questions. Either order is `at` then `id` — already total,
    /// since `id` is the primary key, which is why `audit_log` needs only a widened index and not a
    /// new sort (decision 9). [`TrailOrder::Oldest`] is that index read backwards, so it needed no
    /// index of its own.
    ///
    /// The order changes which page a row lands on, never which rows match or how many: `total` is
    /// the same either way.
    ///
    /// `total` counts every matching row, not the page, and on an append-only log that count is the
    /// expensive part of this read: the database walks the whole matching range to produce it. The
    /// console always names a tenant or a time bound, so the range it counts is bounded in practice;
    /// a caller that filters by nothing on a log of millions is the case for keyset paging, which
    /// ADR-0098 deliberately did not decide.
    ///
    /// # Errors
    ///
    /// [`AuditStoreError`] if the entries could not be read.
    fn query_page(
        &self,
        filter: &AuditQuery,
        page: PageRequest,
        order: TrailOrder,
    ) -> impl Future<Output = Result<Page<AuditEntry>, AuditStoreError>> + Send;
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

#[cfg(test)]
mod tests {
    use super::TrailOrder;

    #[test]
    fn a_trail_order_token_round_trips_and_an_unknown_one_is_not_read_as_a_default() {
        for order in [TrailOrder::Newest, TrailOrder::Oldest] {
            assert_eq!(
                TrailOrder::from_token(order.as_token()),
                Some(order),
                "every token this type prints is a token it reads",
            );
        }
        assert_eq!(
            TrailOrder::from_token("asc"),
            None,
            "the direction vocabulary the item read uses is not silently accepted here — it would \
             mean the opposite of what a caller expects on half the routes",
        );
        assert_eq!(
            TrailOrder::tokens().len(),
            2,
            "the list the refusal shows a caller covers every variant",
        );
        for order in [TrailOrder::Newest, TrailOrder::Oldest] {
            assert!(
                TrailOrder::tokens().contains(&order.as_token()),
                "{order:?} is offered to callers, not only accepted from them",
            );
        }
    }

    #[test]
    fn the_default_order_is_the_one_the_trail_had_before_the_order_existed() {
        assert_eq!(
            TrailOrder::default(),
            TrailOrder::Newest,
            "an absent `?order=` must answer exactly what the route answered before",
        );
    }
}
