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
//! - **[`AssignmentStore`]** — the join binding a person to one of their tenant's stores with a role.
//!   Removing an assignment offboards the person from that store without touching the person, so —
//!   unlike employees and roles — an assignment is a plain grant that is *removed*, not archived.
//!
//! None of this is PII beyond the employee row itself: a role template is names + permission ids, an
//! assignment is three ids. Tenant isolation is the explicit `tenant_id` column + RLS every cloud table
//! carries; both sides of an assignment are the same tenant by that isolation plus the route-layer
//! referential checks (ADR-0070).

use core::fmt;
use core::future::Future;

use serde::Serialize;

use pos_core::permission::Permission;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::ulid::Ulid;

use crate::paging::{Page, PageRequest};
use crate::registry::EntityStatus;
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

/// Persists and reads a tenant's employees. The PIN is **set/reset, never read**: the caller hashes
/// it with Argon2id and passes the PHC to [`set_pin`](Self::set_pin); [`pin_phc`](Self::pin_phc)
/// returns the stored hash only for the trusted publish path (compiling the store's permission node)
/// and tests, never over the API.
pub trait EmployeeStore {
    /// Inserts an employee (no PIN yet).
    ///
    /// # Errors
    ///
    /// [`EmployeeStoreError`] if the write fails (including a duplicate `code` within the tenant).
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
    /// # Errors
    ///
    /// [`EmployeeStoreError`] if the read fails.
    fn list_page(
        &self,
        tenant: TenantId,
        page: PageRequest,
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

/// A failure of the employee store itself — the database is unreachable, or a write violated a
/// constraint (e.g. a duplicate staff code within the tenant).
#[derive(Debug, thiserror::Error)]
#[error("the employee store failed: {0}")]
pub struct EmployeeStoreError(String);

impl EmployeeStoreError {
    /// Wraps a message (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
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
    /// The granted permission ids, each a `pos-core` catalogue id.
    pub permissions: Vec<String>,
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
    /// The granted permission ids (validated against the catalogue by the caller).
    pub permissions: Vec<String>,
}

/// An update to a role template's name, permission set, and/or status.
#[derive(Debug, Clone)]
pub struct RoleTemplateUpdate {
    /// The template to change.
    pub role_template_id: RoleTemplateId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The new name.
    pub name: String,
    /// The new permission set.
    pub permissions: Vec<String>,
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

    /// Updates a role template's name, permissions, and status. Applies only at `expected`.
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

/// A person's assignment to a store with a role. All three ids are the same tenant (the `tenant_id`
/// column + RLS isolate them; the route layer checks referential validity before writing).
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
/// `console.people.manage`, the same gate as reading the roster.
#[derive(Debug, Clone, Serialize)]
pub struct Assignment {
    /// The assignment id.
    pub assignment_id: AssignmentId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The assigned employee.
    pub employee_id: EmployeeId,
    /// The store they work at.
    pub store_id: StoreId,
    /// The role that store grants them.
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
    /// The store.
    pub store_id: StoreId,
    /// The role that store grants.
    pub role_template_id: RoleTemplateId,
}

/// Persists and reads per-store assignments. Unlike employees and roles, an assignment is a grant that
/// is **removed** (offboarding from a store), not archived.
pub trait AssignmentStore {
    /// Assigns an employee to a store with a role.
    ///
    /// # Errors
    ///
    /// [`AssignmentStoreError`] if the write fails (including a duplicate employee-at-store within the
    /// tenant).
    fn assign(
        &self,
        assignment: &NewAssignment,
    ) -> impl Future<Output = Result<(), AssignmentStoreError>> + Send;

    /// Lists the assignments at a store.
    ///
    /// # Errors
    ///
    /// [`AssignmentStoreError`] if the read fails.
    fn list_for_store(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Vec<Assignment>, AssignmentStoreError>> + Send;

    /// Lists the stores a person is assigned to.
    ///
    /// # Errors
    ///
    /// [`AssignmentStoreError`] if the read fails.
    fn list_for_employee(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> impl Future<Output = Result<Vec<Assignment>, AssignmentStoreError>> + Send;

    /// Removes an assignment (offboards the person from that store). Returns whether a row was removed.
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

/// A failure of the assignment store — the database is unreachable or a write violated a constraint
/// (e.g. the same employee assigned to the same store twice).
#[derive(Debug, thiserror::Error)]
#[error("the assignment store failed: {0}")]
pub struct AssignmentStoreError(String);

impl AssignmentStoreError {
    /// Wraps a message (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}
