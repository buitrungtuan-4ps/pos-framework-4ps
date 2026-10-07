// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The people & access seam ([ADR-0070](../../../docs/adr/0070-people-and-access.md), Track M1).
//!
//! A store's **employees**: the console's first record of who works there and — set separately — the
//! PIN they sign in with at the edge. This is the console's first **T1 Restricted** data (a person's
//! name, staff code, PIN), so the seam is deliberately narrow: it stores only what access control
//! needs, it never returns a PIN (only whether one is *set*), and the PIN is held only as its
//! **Argon2id** hash — the same primitive the admin password uses, never the digits themselves. It is
//! access management, not employee monitoring: there is no contact, biometric, behavioural, or
//! location data here (ADR-0070).
//!
//! A trait so it runs against an in-memory fake in tests and the tenant-scoped, RLS-isolated
//! `employees` table in the cloud (the impl lives in [`crate::persistence`], the SQL in
//! `store-postgres`). Employees are **archived, never hard-deleted**, so a published permission set and
//! any history stay reconcilable and erasure is handled through the Data Protection contact
//! ([ADR-0035](../../../docs/adr/0035-retention-and-pii-masking.md)), not an ad-hoc delete.
//!
//! Beside employees this module carries the two seams that give them *access*:
//! - **[`RoleTemplateStore`]** — a tenant's named roles (*Cashier*, *Manager*, …), each a stored subset
//!   of the **`pos-core` permission catalogue** (§9). The console never invents a permission string; it
//!   offers [`permission_catalogue`] and stores a subset [`is_known_permission`] accepts. Templates are
//!   archived, never deleted, like employees.
//! - **[`AssignmentStore`]** — the join binding a person with a role to one of their tenant's stores,
//!   a store group, or every store ([`AssignmentScope`]). Removing an assignment offboards the person
//!   from where it reached without touching the person, so — unlike employees and roles — an
//!   assignment is a plain grant that is *removed*, not archived.
//!
//! None of this is PII beyond the employee row itself: a role template is names + permission ids, an
//! assignment is ids and a scope. Tenant isolation is the explicit `tenant_id` column + RLS every
//! cloud table carries; both sides of an assignment are the same tenant by that isolation plus the
//! route-layer referential checks (ADR-0070).

use core::fmt;
use core::future::Future;

use serde::Serialize;

use pos_core::permission::Permission;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::WireEnum;

use crate::paging::{Page, PageRequest};
use crate::registry::EntityStatus;
use crate::store_groups::StoreGroupId;
use crate::version::{UpdateOutcome, Version, Versioned};

/// An employee's identifier — a ULID minted at creation. Defined here beside the seam, like
/// [`BrandId`](crate::registry::BrandId): an employee is a cloud-only concept, so it needs no
/// `pos-proto` id type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct EmployeeId(Ulid);

impl EmployeeId {
    /// Wraps a ULID as an employee id.
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

impl fmt::Display for EmployeeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// An employee as the console reads them. Note what is **absent**: no PIN and no PIN hash — a read
/// exposes only `has_pin`, whether a PIN is set, so the directory never becomes a way to exfiltrate
/// credentials. Serializes for the `/admin` read (a later slice).
#[derive(Debug, Clone, Serialize)]
pub struct Employee {
    /// The employee's id.
    pub employee_id: EmployeeId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The staff/badge code an operator types — unique within the tenant.
    pub code: String,
    /// The person's name.
    pub name: String,
    /// Active or archived.
    pub status: EntityStatus,
    /// Whether a sign-in PIN is set. The hash itself is never read out.
    pub has_pin: bool,
}

/// A new employee to create — identity only; a PIN is set separately with [`EmployeeStore::set_pin`],
/// and the status starts active.
#[derive(Debug, Clone)]
pub struct NewEmployee {
    /// The minted id.
    pub employee_id: EmployeeId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The staff code.
    pub code: String,
    /// The person's name.
    pub name: String,
}

/// An update to an employee's name and/or status, addressed by id within its tenant.
#[derive(Debug, Clone)]
pub struct EmployeeUpdate {
    /// The employee to change.
    pub employee_id: EmployeeId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The new name.
    pub name: String,
    /// The new status (archiving offboards without deleting).
    pub status: EntityStatus,
}

/// The orders `GET /admin/employees` offers a page in.
///
/// A closed set rather than a column name from the caller: the wire token maps to a `&'static str`
/// `ORDER BY` in the adapter through an exhaustive match, so a caller can never name a column, and a
/// new variant here breaks that mapping at compile time.
///
/// Every variant's order is *total* — each ends with the primary key. `ORDER BY name` is no more
/// total than `ORDER BY created_at` was: two employees can share a name, which is one of the reasons
/// a staff code exists, and a window over that tie can repeat or skip a row across pages just the
/// same. [ADR-0098](../../docs/adr/0098-paged-admin-reads.md) decision 9 applies to every sort a
/// route offers, not only its default.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum EmployeeSort {
    /// Newest first — the order the unpaged roster read has always used, and the default.
    #[default]
    Newest,
    /// By name, for finding a person rather than reviewing recent hires.
    Name,
    /// By staff code, which is what a badge carries.
    Code,
}

impl EmployeeSort {
    /// The wire token for this order, as `?sort=` sends it.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Newest => "newest",
            Self::Name => "name",
            Self::Code => "code",
        }
    }

    /// Reads a wire token, or `None` when it is not one this read offers.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "newest" => Some(Self::Newest),
            "name" => Some(Self::Name),
            "code" => Some(Self::Code),
            _unknown => None,
        }
    }

    /// Every token, for the refusal that tells a caller what it may send.
    #[must_use]
    pub const fn tokens() -> &'static [&'static str] {
        &["newest", "name", "code"]
    }
}

/// What a caller wants of a page of employees beyond its bounds: which rows, in what order.
///
/// Separate from [`PageRequest`] for the reason [`ItemListFilter`](crate::catalog::ItemListFilter)
/// gives: `PageRequest` is the vocabulary every paged read shares, and the reads that cannot search
/// or sort would silently ignore these — the class of quiet wrong answer ADR-0098 exists to prevent.
///
/// **This is T1 personal data on both sides.** The search runs over a person's name and code, and
/// the page it shapes carries them back. Neither the filter nor the order reaches a log: an audit
/// entry for a read would record that a roster was read, never what was typed to find someone
/// ([ADR-0070](../../docs/adr/0070-people-and-access.md)).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct EmployeeListFilter {
    /// Case-insensitive substring the person's name or staff code must contain. `None` matches all.
    pub search: Option<String>,
    /// The order the page comes back in.
    pub sort: EmployeeSort,
    /// Whether `sort` runs the other way. `Newest` is newest-first by nature; this inverts it.
    pub descending: bool,
}

/// Persists and reads a tenant's employees. The PIN is **set/reset, never read**: the caller hashes
/// it with Argon2id and passes the PHC to [`set_pin`](Self::set_pin); [`pin_phc`](Self::pin_phc)
/// returns the stored hash only for the trusted publish path (compiling the store's permission node)
/// and tests, never over the API.
pub trait EmployeeStore {
    /// Inserts an employee (no PIN yet).
    ///
    /// # Errors
    ///
    /// [`EmployeeStoreError::CodeInUse`] if the tenant already has an employee with that `code`,
    /// which writes nothing; [`EmployeeStoreError`] of the other kind if the write fails.
    fn create(
        &self,
        employee: &NewEmployee,
    ) -> impl Future<Output = Result<Version, EmployeeStoreError>> + Send;

    /// Lists a tenant's employees, newest first.
    ///
    /// # Errors
    ///
    /// [`EmployeeStoreError`] if the read fails.
    fn list(
        &self,
        tenant: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<Employee>>, EmployeeStoreError>> + Send;

    /// One page of a tenant's employees, newest first, with the headcount.
    ///
    /// Beside [`list`](Self::list) rather than replacing it, for the reason
    /// [ADR-0098](../../../docs/adr/0098-paged-admin-reads.md) gives: the publish path compiles the
    /// permission node from the whole roster and a node built from a page would be missing whoever
    /// fell off it. The console's table is the caller that wants a page.
    ///
    /// **This is T1 personal data and paging does not change that** ([ADR-0070](../../../docs/adr/0070-people-and-access.md)).
    /// It changes only *how much of the roster crosses the wire at once* — the same fields behind the
    /// same `console.people.manage` gate, and strictly less data per response than the read it sits
    /// beside. No field is added, and nothing new reaches a log.
    ///
    /// The order is `created_at DESC, id DESC` — total, because `id` is the primary key. It has to
    /// be: an import writes a whole roster in one transaction, and PostgreSQL's `now()` is
    /// transaction time, so `created_at` alone does not order those rows (decision 9).
    ///
    /// `filter` carries the search and the order. Its `search` is a case-insensitive substring the
    /// person's **name or staff code** must contain — the two things an operator knows about someone
    /// they are looking for — and `total` counts what it matched rather than the whole roster, so a
    /// pager sizes itself to the result. It does not search `pin_phc`, which is not selected by any
    /// read and would be meaningless to match a substring against besides.
    ///
    /// `sort`/`descending` pick the order. Every one of them is total
    /// ([ADR-0098](../../docs/adr/0098-paged-admin-reads.md) decision 9): the default order is not
    /// the only one a window can straddle a tie in.
    ///
    /// # Errors
    ///
    /// [`EmployeeStoreError`] if the read fails.
    fn list_page(
        &self,
        tenant: TenantId,
        page: PageRequest,
        filter: &EmployeeListFilter,
    ) -> impl Future<Output = Result<Page<Versioned<Employee>>, EmployeeStoreError>> + Send;

    /// Reads one employee within its tenant, or `None` if there is no such id.
    ///
    /// # Errors
    ///
    /// [`EmployeeStoreError`] if the read fails.
    fn get(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> impl Future<Output = Result<Option<Versioned<Employee>>, EmployeeStoreError>> + Send;

    /// Renames an employee and/or sets their status, within their tenant. Applies only at
    /// `expected`; the outcome tells a handler whether to answer `404` or `412`.
    ///
    /// # Errors
    ///
    /// [`EmployeeStoreError`] if the write fails.
    fn update(
        &self,
        employee: &EmployeeUpdate,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, EmployeeStoreError>> + Send;

    /// Sets (or resets) an employee's sign-in PIN to the given **Argon2id PHC hash**, within their
    /// tenant. The caller hashes; this never sees the digits.
    ///
    /// Deliberately **not** version-gated ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)):
    /// it writes one field that no other console form edits, so there is no edit for it to clobber.
    /// It does still *move* the row's version, because it is a write — a caller holding a version
    /// from before it must re-read, which is what the console does after every write.
    ///
    /// # Errors
    ///
    /// [`EmployeeStoreError`] if the write fails.
    fn set_pin(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
        pin_phc: &str,
    ) -> impl Future<Output = Result<bool, EmployeeStoreError>> + Send;

    /// The stored Argon2id PHC hash of an employee's PIN, or `None` if the employee is unknown or has
    /// no PIN set. For the trusted publish path (compiling the store's permission node) and tests —
    /// never returned over the API.
    ///
    /// # Errors
    ///
    /// [`EmployeeStoreError`] if the read fails.
    fn pin_phc(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> impl Future<Output = Result<Option<String>, EmployeeStoreError>> + Send;
}

/// A failure of the employee store: the staff code is already in use in the tenant, or the store
/// itself failed.
///
/// The two are kept apart, as [`AssignmentStoreError`]'s are, because they ask different things of
/// the caller. A code in use is a conflict the operator resolves by choosing another code, and a
/// retry can never succeed; a failure of the store is an outage, and a retry may.
#[derive(Debug, thiserror::Error)]
pub enum EmployeeStoreError {
    /// The tenant already has an employee with this staff code. Nothing was written.
    ///
    /// It carries no text, unlike [`AssignmentStoreError::AlreadyAssigned`]: a staff code identifies
    /// a person, so its value has no place in an error that may reach a log, and the variant already
    /// says everything there is to say.
    #[error("the staff code is already in use in the tenant")]
    CodeInUse,
    /// The store could not read or write: the database is unreachable, or a row would not parse.
    #[error("the employee store failed: {0}")]
    Failed(String),
}

impl EmployeeStoreError {
    /// A failure of the store, wrapping a message (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }
}

// --- role templates: named permission sets drawn from the pos-core catalogue (§9) ---

/// One entry of the permission catalogue the console offers when authoring a role. Mirrors the
/// `pos-core` [`Permission`] metadata (§9) so the console renders permissions grouped, with their
/// risk and PIN policy, from the framework's own source of truth rather than a hand-kept UI list.
#[derive(Debug, Clone, Serialize)]
pub struct PermissionInfo {
    /// The stable `domain.resource.action` id a role template stores.
    pub id: &'static str,
    /// The group token the dashboard groups by (e.g. `BILLING`).
    pub group: &'static str,
    /// The risk token (`LOW`/`MEDIUM`/`HIGH`).
    pub risk: &'static str,
    /// Whether the permission always demands a PIN at the point of use, whatever the role.
    pub pin_required: bool,
    /// A one-line description for the dashboard.
    pub description: &'static str,
}

/// The full `pos-core` permission catalogue (§9), in declaration order — the fixed set a role template
/// may draw from. The console offers these and stores a subset; it never invents a string.
#[must_use]
pub fn permission_catalogue() -> Vec<PermissionInfo> {
    Permission::ALL
        .iter()
        .map(|permission| {
            let meta = permission.meta();
            PermissionInfo {
                id: meta.id,
                group: meta.group.as_token(),
                risk: meta.risk.as_token(),
                pin_required: meta.pin_required,
                description: meta.description,
            }
        })
        .collect()
}

/// Whether `id` is a known `pos-core` permission id. Used to reject a role template that names a
/// permission outside the catalogue (§9) before it is stored.
#[must_use]
pub fn is_known_permission(id: &str) -> bool {
    Permission::ALL
        .iter()
        .any(|permission| permission.meta().id == id)
}

/// Whether `id` is a `pos-core` permission whose catalogue entry is PIN-flagged.
///
/// Only such a permission may be granted **with approval**
/// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
/// decision 4). The flag is also where the console's role editor starts a newly ticked permission,
/// and nothing more: the route stores what it is sent. An unknown id is not PIN-flagged.
#[must_use]
pub fn is_pin_flagged_permission(id: &str) -> bool {
    Permission::ALL
        .iter()
        .any(|permission| permission.meta().id == id && permission.meta().pin_required)
}

/// A role template's identifier — a ULID minted at creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct RoleTemplateId(Ulid);

impl RoleTemplateId {
    /// Wraps a ULID as a role-template id.
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

impl fmt::Display for RoleTemplateId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A tenant's named role — a stored subset of the `pos-core` permission catalogue (§9). One tenant's
/// *Manager* is not another's, so templates are per-tenant. Archived, never deleted, so an assignment
/// or published permission set that references it stays reconcilable.
#[derive(Debug, Clone, Serialize)]
pub struct RoleTemplate {
    /// The template's id.
    pub role_template_id: RoleTemplateId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The human role name (e.g. *Cashier*), unique within the tenant.
    pub name: String,
    /// The permission ids the role grants **directly**, each a `pos-core` catalogue id: its holder
    /// acts on them alone.
    pub permissions: Vec<String>,
    /// The permission ids the role grants only **with approval**: another person who holds one
    /// directly enters their code and PIN for each act
    /// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
    /// decision 4). Each is a PIN-flagged catalogue id, and none is also in [`Self::permissions`].
    /// Always on the wire, empty for a role that grants everything directly.
    pub permissions_with_approval: Vec<String>,
    /// How much this role may discount before it needs a manager, in the currency's minor unit.
    ///
    /// `billing.discount.apply` is granted to a **server** and is not PIN-flagged; its own
    /// description is *"apply a discount up to the role's configured ceiling"*, and until this field
    /// existed there was nowhere to configure one. The edge reads an absent ceiling as **zero**
    /// rather than as "no limit" — the safe direction — so a role that leaves this `None` keeps
    /// today's behaviour exactly: every discount needs `billing.discount.override_ceiling` and a
    /// manager's PIN.
    ///
    /// `None` and `Some(0)` land in the same place and are not the same statement. The first is an
    /// absence; the second is a tenant saying this role discounts nothing. Never negative — the
    /// column refuses it, and so does the route.
    pub discount_ceiling_minor: Option<i64>,
    /// Active or archived.
    pub status: EntityStatus,
}

/// A new role template to create.
#[derive(Debug, Clone)]
pub struct NewRoleTemplate {
    /// The minted id.
    pub role_template_id: RoleTemplateId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The role name.
    pub name: String,
    /// The permission ids granted directly (validated against the catalogue by the caller).
    pub permissions: Vec<String>,
    /// The permission ids granted only with approval (ADR-0158 decision 4): PIN-flagged, none also
    /// in [`Self::permissions`] — both checked by the caller.
    pub permissions_with_approval: Vec<String>,
    /// How much this role may discount before it needs a manager, in the currency's minor unit.
    ///
    /// `billing.discount.apply` is granted to a **server** and is not PIN-flagged; its own
    /// description is *"apply a discount up to the role's configured ceiling"*, and until this field
    /// existed there was nowhere to configure one. The edge reads an absent ceiling as **zero**
    /// rather than as "no limit" — the safe direction — so a role that leaves this `None` keeps
    /// today's behaviour exactly: every discount needs `billing.discount.override_ceiling` and a
    /// manager's PIN.
    ///
    /// `None` and `Some(0)` land in the same place and are not the same statement. The first is an
    /// absence; the second is a tenant saying this role discounts nothing. Never negative — the
    /// column refuses it, and so does the route.
    pub discount_ceiling_minor: Option<i64>,
}

/// An update to a role template's name, permission sets, ceiling and/or status.
#[derive(Debug, Clone)]
pub struct RoleTemplateUpdate {
    /// The template to change.
    pub role_template_id: RoleTemplateId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The new name.
    pub name: String,
    /// The new set granted directly.
    pub permissions: Vec<String>,
    /// The new set granted only with approval (ADR-0158 decision 4): PIN-flagged, none also in
    /// [`Self::permissions`] — both checked by the caller.
    pub permissions_with_approval: Vec<String>,
    /// How much this role may discount before it needs a manager, in the currency's minor unit.
    ///
    /// `billing.discount.apply` is granted to a **server** and is not PIN-flagged; its own
    /// description is *"apply a discount up to the role's configured ceiling"*, and until this field
    /// existed there was nowhere to configure one. The edge reads an absent ceiling as **zero**
    /// rather than as "no limit" — the safe direction — so a role that leaves this `None` keeps
    /// today's behaviour exactly: every discount needs `billing.discount.override_ceiling` and a
    /// manager's PIN.
    ///
    /// `None` and `Some(0)` land in the same place and are not the same statement. The first is an
    /// absence; the second is a tenant saying this role discounts nothing. Never negative — the
    /// column refuses it, and so does the route.
    pub discount_ceiling_minor: Option<i64>,
    /// The new status (archiving retires the role without deleting it).
    pub status: EntityStatus,
}

/// Persists and reads a tenant's role templates. Archived, never deleted.
pub trait RoleTemplateStore {
    /// Inserts a role template.
    ///
    /// # Errors
    ///
    /// [`RoleTemplateStoreError`] if the write fails (including a duplicate `name` within the tenant).
    fn create(
        &self,
        template: &NewRoleTemplate,
    ) -> impl Future<Output = Result<Version, RoleTemplateStoreError>> + Send;

    /// Lists a tenant's role templates, newest first.
    ///
    /// # Errors
    ///
    /// [`RoleTemplateStoreError`] if the read fails.
    fn list(
        &self,
        tenant: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<RoleTemplate>>, RoleTemplateStoreError>> + Send;

    /// Reads one role template within its tenant, or `None`.
    ///
    /// # Errors
    ///
    /// [`RoleTemplateStoreError`] if the read fails.
    fn get(
        &self,
        tenant: TenantId,
        role_template_id: RoleTemplateId,
    ) -> impl Future<Output = Result<Option<Versioned<RoleTemplate>>, RoleTemplateStoreError>> + Send;

    /// Updates a role template's name, both permission sets, ceiling and status. Applies only at
    /// `expected`.
    ///
    /// # Errors
    ///
    /// [`RoleTemplateStoreError`] if the write fails.
    fn update(
        &self,
        template: &RoleTemplateUpdate,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, RoleTemplateStoreError>> + Send;
}

/// A failure of the role-template store — the database is unreachable or a write violated a constraint
/// (e.g. a duplicate role name within the tenant).
#[derive(Debug, thiserror::Error)]
#[error("the role-template store failed: {0}")]
pub struct RoleTemplateStoreError(String);

impl RoleTemplateStoreError {
    /// Wraps a message (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

// --- assignments: bind a person to a store with a role ---

/// An assignment's identifier — a ULID minted at creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct AssignmentId(Ulid);

impl AssignmentId {
    /// Wraps a ULID as an assignment id.
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

impl fmt::Display for AssignmentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

pos_proto::wire_enum! {
    /// Which stores an assignment reaches
    /// ([ADR-0158](../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
    /// decision 3): one, every store of a group, or every store of the tenant. The wire's
    /// `scope_kind`. `ASSIGNMENT_SCOPE_UNSPECIFIED` is the zero value every wire enum has
    /// (`docs/naming-and-api.md` §3.3), and no assignment is held at it.
    AssignmentScopeKind, prefix = "ASSIGNMENT_SCOPE";
    /// One store, named by `store_id`.
    Store = "STORE",
    /// Every store a group holds, named by `store_group_id`.
    StoreGroup = "STORE_GROUP",
    /// Every store of the tenant.
    Tenant = "TENANT",
}

/// Where an assignment grants its role: its [`AssignmentScopeKind`] with the id that names it.
///
/// A group assignment reaches the stores the group holds when it is read, so a store that joins
/// the group is reached and one that leaves is not
/// ([ADR-0122](../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md)). A tenant-wide one
/// reaches every store of the tenant, a store opened after it included.
///
/// On the wire it is `scope_kind` with the id beside it: `store_id` for one store, where it always
/// was, so a reader that predates the scopes reads a one-store assignment unchanged;
/// `store_group_id` for a group; and neither for the tenant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssignmentScope {
    /// One store.
    Store(StoreId),
    /// Every store the group holds.
    StoreGroup(StoreGroupId),
    /// Every store of the tenant.
    Tenant,
}

impl AssignmentScope {
    /// The kind, as `scope_kind` carries it.
    #[must_use]
    pub const fn kind(self) -> AssignmentScopeKind {
        match self {
            Self::Store(_) => AssignmentScopeKind::Store,
            Self::StoreGroup(_) => AssignmentScopeKind::StoreGroup,
            Self::Tenant => AssignmentScopeKind::Tenant,
        }
    }

    /// The store a one-store assignment names, and `None` for a wider one.
    #[must_use]
    pub const fn store_id(self) -> Option<StoreId> {
        match self {
            Self::Store(store_id) => Some(store_id),
            Self::StoreGroup(_) | Self::Tenant => None,
        }
    }

    /// The group a group assignment names, and `None` otherwise.
    #[must_use]
    pub const fn store_group_id(self) -> Option<StoreGroupId> {
        match self {
            Self::StoreGroup(store_group_id) => Some(store_group_id),
            Self::Store(_) | Self::Tenant => None,
        }
    }
}

impl Serialize for AssignmentScope {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct as _;

        let mut fields = serializer.serialize_struct("AssignmentScope", 2)?;
        fields.serialize_field("scope_kind", self.kind().as_wire())?;
        match self {
            Self::Store(store_id) => fields.serialize_field("store_id", store_id)?,
            Self::StoreGroup(store_group_id) => {
                fields.serialize_field("store_group_id", store_group_id)?;
            }
            Self::Tenant => {}
        }
        fields.end()
    }
}

/// A person's assignment with a role, at one store, a store group or every store of the tenant
/// ([`AssignmentScope`]). Every id is the same tenant (the `tenant_id` column + RLS isolate them;
/// the route layer checks referential validity before writing).
///
/// # Who the assignment is for
///
/// The row carries the assigned person's name and code alongside their id, resolved by the store as
/// it reads. Without them a caller can only name the person by looking the id up in the tenant's
/// roster, which is a whole-set read the console cannot keep making once the roster is paged
/// ([ADR-0098](../../docs/adr/0098-paged-admin-reads.md), B3-4).
///
/// Both are `Option`, and the reason is in the schema rather than the domain: nothing declares a
/// foreign key from an assignment to an employee, so an assignment can outlive the row it names.
/// `None` means exactly that, and a caller should fall back to showing the id — the assignment is
/// real and still grants access, so hiding it would be worse than showing it unlabelled.
///
/// The name and code are T1 personal data
/// ([ADR-0070](../../docs/adr/0070-people-and-access.md)) and reaching them needs
/// `console.people.read`, the same gate as reading the roster
/// ([ADR-0158](../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)).
#[derive(Debug, Clone, Serialize)]
pub struct Assignment {
    /// The assignment id.
    pub assignment_id: AssignmentId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The assigned employee.
    pub employee_id: EmployeeId,
    /// Where it grants the role: `scope_kind`, and `store_id` or `store_group_id` beside it.
    #[serde(flatten)]
    pub scope: AssignmentScope,
    /// The role it grants wherever it reaches.
    pub role_template_id: RoleTemplateId,
    /// The assigned person's name, or `None` if no employee row matches `employee_id`.
    pub employee_name: Option<String>,
    /// The assigned person's staff code, `None` on the same terms as the name.
    pub employee_code: Option<String>,
}

/// A new assignment to create.
#[derive(Debug, Clone)]
pub struct NewAssignment {
    /// The minted id.
    pub assignment_id: AssignmentId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The employee to assign.
    pub employee_id: EmployeeId,
    /// Where the role is granted.
    pub scope: AssignmentScope,
    /// The role it grants.
    pub role_template_id: RoleTemplateId,
}

/// Persists and reads assignments. Unlike employees and roles, an assignment is a grant that is
/// **removed** (offboarding), not archived.
///
/// A person holds at most one assignment per store, one per group and one tenant-wide; the scopes
/// do not exclude each other, so a person may hold a store's assignment and a tenant-wide one at
/// the same store, and the compiler gives them the union.
pub trait AssignmentStore {
    /// Assigns an employee with a role, at the assignment's scope.
    ///
    /// # Errors
    ///
    /// [`AssignmentStoreError::AlreadyAssigned`] for a second assignment of the same employee at the
    /// same store or group, or tenant-wide twice, which writes nothing; [`AssignmentStoreError`] of
    /// the other kind if the write fails.
    fn assign(
        &self,
        assignment: &NewAssignment,
    ) -> impl Future<Output = Result<(), AssignmentStoreError>> + Send;

    /// Lists every assignment that reaches a store: the store's own, those naming a group the
    /// store is a member of, and the tenant-wide ones — everyone who works there, which is what the
    /// store's `permissions` node is compiled from.
    ///
    /// # Errors
    ///
    /// [`AssignmentStoreError`] if the read fails.
    fn list_for_store(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Vec<Assignment>, AssignmentStoreError>> + Send;

    /// Lists the assignments that name a store group — the ones a change to its membership moves.
    ///
    /// # Errors
    ///
    /// [`AssignmentStoreError`] if the read fails.
    fn list_for_group(
        &self,
        tenant: TenantId,
        store_group_id: StoreGroupId,
    ) -> impl Future<Output = Result<Vec<Assignment>, AssignmentStoreError>> + Send;

    /// Lists the tenant-wide assignments.
    ///
    /// # Errors
    ///
    /// [`AssignmentStoreError`] if the read fails.
    fn list_tenant_wide(
        &self,
        tenant: TenantId,
    ) -> impl Future<Output = Result<Vec<Assignment>, AssignmentStoreError>> + Send;

    /// Lists a person's assignments, of every scope.
    ///
    /// # Errors
    ///
    /// [`AssignmentStoreError`] if the read fails.
    fn list_for_employee(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> impl Future<Output = Result<Vec<Assignment>, AssignmentStoreError>> + Send;

    /// Lists the assignments that grant a role, of every scope.
    ///
    /// The stores these reach are the ones archiving the role reaches
    /// ([ADR-0158](../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
    /// decision 7): each must be told at once that the role no longer grants anything.
    ///
    /// # Errors
    ///
    /// [`AssignmentStoreError`] if the read fails.
    fn list_for_role(
        &self,
        tenant: TenantId,
        role_template_id: RoleTemplateId,
    ) -> impl Future<Output = Result<Vec<Assignment>, AssignmentStoreError>> + Send;

    /// Reads one assignment within its tenant, or `None`.
    ///
    /// Read before a removal, which needs the scope the grant was at: every store it reaches is
    /// told at once (ADR-0158 decision 7), and the row will be gone by the time anyone asks.
    ///
    /// # Errors
    ///
    /// [`AssignmentStoreError`] if the read fails.
    fn get(
        &self,
        tenant: TenantId,
        assignment_id: AssignmentId,
    ) -> impl Future<Output = Result<Option<Assignment>, AssignmentStoreError>> + Send;

    /// Removes an assignment (offboards the person from where it reached). Returns whether a row was
    /// removed.
    ///
    /// # Errors
    ///
    /// [`AssignmentStoreError`] if the write fails.
    fn remove(
        &self,
        tenant: TenantId,
        assignment_id: AssignmentId,
    ) -> impl Future<Output = Result<bool, AssignmentStoreError>> + Send;
}

/// A failure of the assignment store: the person already holds an assignment where a new one would
/// go, or the store itself failed.
///
/// The two are kept apart because they ask different things of the caller. A duplicate is a
/// conflict the operator resolves by changing or removing the assignment that is there, and a retry
/// can never succeed; a failure of the store is an outage, and a retry may.
#[derive(Debug, thiserror::Error)]
pub enum AssignmentStoreError {
    /// The person already holds an assignment at that scope: one per store, one per group and one
    /// tenant-wide. Nothing was written.
    #[error("the employee is already assigned there: {0}")]
    AlreadyAssigned(String),
    /// The store could not read or write: the database is unreachable, or a row would not parse.
    #[error("the assignment store failed: {0}")]
    Failed(String),
}

impl AssignmentStoreError {
    /// A failure of the store, wrapping a message (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Assignment, AssignmentId, AssignmentScope, EmployeeId, RoleTemplateId,
        is_pin_flagged_permission, permission_catalogue,
    };
    use crate::store_groups::StoreGroupId;
    use pos_core::permission::Permission;
    use pos_proto::ids::{StoreId, TenantId};
    use pos_proto::ulid::Ulid;

    /// A one-store assignment reads as it did, with `scope_kind` added before the `store_id` it
    /// always carried; a group's names its group instead, and a tenant-wide one names neither
    /// (ADR-0158 decision 3).
    #[test]
    fn an_assignment_carries_its_scope_beside_the_id_it_names() {
        let at = |scope| Assignment {
            assignment_id: AssignmentId::new(Ulid::from_u128(1)),
            tenant_id: TenantId::new(Ulid::from_u128(2)),
            employee_id: EmployeeId::new(Ulid::from_u128(3)),
            scope,
            role_template_id: RoleTemplateId::new(Ulid::from_u128(4)),
            employee_name: None,
            employee_code: None,
        };
        let id = |n: u128| Ulid::from_u128(n).to_string();
        let store = AssignmentScope::Store(StoreId::new(Ulid::from_u128(5)));
        assert_eq!(
            serde_json::to_string(&at(store)).expect("serialise"),
            format!(
                "{{\"assignment_id\":\"{}\",\"tenant_id\":\"{}\",\"employee_id\":\"{}\",\
                 \"scope_kind\":\"ASSIGNMENT_SCOPE_STORE\",\"store_id\":\"{}\",\
                 \"role_template_id\":\"{}\",\"employee_name\":null,\"employee_code\":null}}",
                id(1),
                id(2),
                id(3),
                id(5),
                id(4)
            )
        );

        let group = serde_json::to_value(at(AssignmentScope::StoreGroup(StoreGroupId::new(
            Ulid::from_u128(6),
        ))))
        .expect("serialise");
        assert_eq!(group["scope_kind"], "ASSIGNMENT_SCOPE_STORE_GROUP");
        assert_eq!(group["store_group_id"], id(6));
        assert!(group.get("store_id").is_none());

        let everywhere = serde_json::to_value(at(AssignmentScope::Tenant)).expect("serialise");
        assert_eq!(everywhere["scope_kind"], "ASSIGNMENT_SCOPE_TENANT");
        assert!(everywhere.get("store_id").is_none());
        assert!(everywhere.get("store_group_id").is_none());
    }

    /// Migration 0074 gave every role that existed each PIN-flagged permission it did not grant
    /// directly, with approval, from a literal list in its SQL: the catalogue's PIN-flagged
    /// permissions as they were then, in byte order and once each. A PIN-flagged permission added
    /// since is granted by a migration of its own, which names it — 0076 for `billing.fee.waive`
    /// (ADR-0159 decision 5) and 0084 for `cash.shift.manage_other_till` (ADR-0167 decision 3).
    /// Together they are the `pin_required=true` lines of
    /// `docs/snapshots/permissions.txt`, each granted once, so the backfills and the catalogue
    /// cannot drift.
    #[test]
    fn the_with_approval_backfills_list_exactly_the_pin_flagged_permissions() {
        const MIGRATION_0074: &str = include_str!(
            "../../adapters/store-postgres/migrations/0074_role_permissions_with_approval.sql"
        );
        const MIGRATION_0076: &str =
            include_str!("../../adapters/store-postgres/migrations/0076_roles_can_waive_a_fee.sql");
        const MIGRATION_0084: &str = include_str!(
            "../../adapters/store-postgres/migrations/0084_roles_can_manage_another_tills_drawer.sql"
        );
        let open = MIGRATION_0074
            .find("ARRAY[")
            .expect("the backfill's literal list")
            + "ARRAY[".len();
        let close = open
            + MIGRATION_0074[open..]
                .find(']')
                .expect("the list is closed");
        let literal: Vec<&str> = MIGRATION_0074[open..close]
            .split(',')
            .map(|item| item.trim().trim_matches('\''))
            .collect();
        let mut sorted = literal.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(literal, sorted, "0074's list is in byte order, once each");

        // Each permission granted by a later migration of its own, and that migration.
        let later = [
            ("billing.fee.waive", MIGRATION_0076),
            ("cash.shift.manage_other_till", MIGRATION_0084),
        ];
        for (id, migration) in later {
            assert!(
                migration.contains(&format!("'{id}'")),
                "the migration that grants {id} names it"
            );
            assert!(!literal.contains(&id), "{id} is granted once");
        }
        let mut granted: Vec<&str> = literal
            .iter()
            .copied()
            .chain(later.iter().map(|(id, _)| *id))
            .collect();
        granted.sort_unstable();
        let mut flagged: Vec<&str> = Permission::ALL
            .iter()
            .map(|permission| permission.meta())
            .filter(|meta| meta.pin_required)
            .map(|meta| meta.id)
            .collect();
        flagged.sort_unstable();
        assert_eq!(granted, flagged);
    }

    /// Migration 0079 gave `reports.takings.view` to every role that closes a shift directly and
    /// grants a PIN-flagged permission directly, from a literal list of the catalogue's PIN-flagged
    /// permissions as they were then, in byte order and once each (ADR-0160 decision 2). A
    /// PIN-flagged permission added since is named here with the migration that grants it, and the
    /// two together are the catalogue's `pin_required` set, so a new one can neither widen 0079's
    /// rule silently nor leave it reading a list that no longer says who approves.
    #[test]
    fn the_takings_grant_lists_exactly_the_pin_flagged_permissions() {
        const MIGRATION_0079: &str = include_str!(
            "../../adapters/store-postgres/migrations/0079_approvers_who_close_a_shift_see_takings.sql"
        );
        const MIGRATION_0084: &str = include_str!(
            "../../adapters/store-postgres/migrations/0084_roles_can_manage_another_tills_drawer.sql"
        );
        let open = MIGRATION_0079
            .find("ARRAY[")
            .expect("the grant's literal list")
            + "ARRAY[".len();
        let close = open
            + MIGRATION_0079[open..]
                .find(']')
                .expect("the list is closed");
        let literal: Vec<&str> = MIGRATION_0079[open..close]
            .split(',')
            .map(|item| item.trim().trim_matches('\''))
            .collect();
        let mut sorted = literal.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(literal, sorted, "0079's list is in byte order, once each");

        // Each PIN-flagged permission added after 0079, and the migration that grants it.
        let later = [("cash.shift.manage_other_till", MIGRATION_0084)];
        for (id, migration) in later {
            assert!(
                migration.contains(&format!("'{id}'")),
                "the migration that grants {id} names it"
            );
            assert!(!literal.contains(&id), "{id} is listed once");
        }
        let mut listed: Vec<&str> = literal
            .iter()
            .copied()
            .chain(later.iter().map(|(id, _)| *id))
            .collect();
        listed.sort_unstable();
        let mut flagged: Vec<&str> = Permission::ALL
            .iter()
            .map(|permission| permission.meta())
            .filter(|meta| meta.pin_required)
            .map(|meta| meta.id)
            .collect();
        flagged.sort_unstable();
        assert_eq!(listed, flagged);
    }

    /// The route's check and the console's catalogue read the same flag.
    #[test]
    fn a_permission_is_pin_flagged_exactly_when_the_catalogue_says_so() {
        for info in permission_catalogue() {
            assert_eq!(
                is_pin_flagged_permission(info.id),
                info.pin_required,
                "{}",
                info.id
            );
        }
        assert!(!is_pin_flagged_permission("not.a.real.permission"));
    }
}
