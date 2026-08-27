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

use core::fmt;
use core::future::Future;

use serde::Serialize;

use pos_proto::ids::TenantId;
use pos_proto::ulid::Ulid;

use crate::registry::EntityStatus;

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
    ) -> impl Future<Output = Result<(), EmployeeStoreError>> + Send;

    /// Lists a tenant's employees, newest first.
    ///
    /// # Errors
    ///
    /// [`EmployeeStoreError`] if the read fails.
    fn list(
        &self,
        tenant: TenantId,
    ) -> impl Future<Output = Result<Vec<Employee>, EmployeeStoreError>> + Send;

    /// Reads one employee within its tenant, or `None` if there is no such id.
    ///
    /// # Errors
    ///
    /// [`EmployeeStoreError`] if the read fails.
    fn get(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> impl Future<Output = Result<Option<Employee>, EmployeeStoreError>> + Send;

    /// Renames an employee and/or sets their status, within their tenant. Returns whether a row
    /// changed (so a handler can answer `404` for an unknown id).
    ///
    /// # Errors
    ///
    /// [`EmployeeStoreError`] if the write fails.
    fn update(
        &self,
        employee: &EmployeeUpdate,
    ) -> impl Future<Output = Result<bool, EmployeeStoreError>> + Send;

    /// Sets (or resets) an employee's sign-in PIN to the given **Argon2id PHC hash**, within their
    /// tenant. The caller hashes; this never sees the digits. Returns whether a row changed.
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
