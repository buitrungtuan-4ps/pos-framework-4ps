// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Employees over PostgreSQL (Track M1, [ADR-0070](../../../docs/adr/0070-people-and-access.md)).
//!
//! One tenant-scoped row per person. This adapter keeps only the SQL and returns plain rows;
//! `pos-cloud` implements its `EmployeeStore` seam over this type. Tenant scoping is the explicit
//! `WHERE tenant_id = $1` filter every cloud adapter carries (the server connects as the trusted pool
//! owner, which bypasses RLS; the migration's policy is the belt-and-suspenders second line). Its
//! methods use distinct verbs from the seam (`insert`/`fetch`/`set`) so the seam impl calls the SQL
//! and never itself.
//!
//! The PIN is **set/reset, never returned over the API**: [`set_pin`](PostgresPeople::set_pin) stores
//! an Argon2id PHC hash the caller computed, and [`pin_phc`](PostgresPeople::pin_phc) reads it back
//! only for the trusted publish path and tests. A list/read never selects `pin_phc` — only whether it
//! is set (`has_pin`).

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// An employee as listed — identity, code, status, and whether a PIN is set. Never the PIN hash.
#[derive(Clone, Debug)]
pub struct EmployeeRow {
    /// The employee id (a ULID string).
    pub id: String,
    /// The owning tenant.
    pub tenant_id: String,
    /// The staff/badge code, unique within the tenant.
    pub code: String,
    /// The person's name.
    pub name: String,
    /// `active` or `archived`.
    pub status: String,
    /// Whether a sign-in PIN is set (`pin_phc IS NOT NULL`).
    pub has_pin: bool,
}

/// The columns a read returns, in a stable order matching [`employee_row`]. `pin_phc` is deliberately
/// absent — reads expose only `pin_phc IS NOT NULL AS has_pin`, never the hash.
const EMPLOYEE_COLUMNS: &str =
    "id, tenant_id, code, name, status, (pin_phc IS NOT NULL) AS has_pin";

/// The employee store over a shared pool. Built by [`PostgresStore::people`](crate::PostgresStore::people).
#[derive(Clone, Debug)]
pub struct PostgresPeople {
    pool: Pool,
}

impl PostgresPeople {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Inserts an employee (no PIN yet).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails (including a
    /// duplicate `(tenant_id, code)`).
    pub async fn insert(
        &self,
        id: &str,
        tenant_id: &str,
        code: &str,
        name: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO employees (id, tenant_id, code, name) VALUES ($1, $2, $3, $4)",
                &[&id, &tenant_id, &code, &name],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a tenant's employees, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch(&self, tenant_id: &str) -> Result<Vec<EmployeeRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {EMPLOYEE_COLUMNS} FROM employees \
                     WHERE tenant_id = $1 ORDER BY created_at DESC"
                ),
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(employee_row).collect())
    }

    /// Reads one employee within its tenant, or `None` if there is no such id.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_one(
        &self,
        tenant_id: &str,
        id: &str,
    ) -> Result<Option<EmployeeRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                &format!(
                    "SELECT {EMPLOYEE_COLUMNS} FROM employees WHERE tenant_id = $1 AND id = $2"
                ),
                &[&tenant_id, &id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.as_ref().map(employee_row))
    }

    /// Renames an employee and sets their status, within their tenant. Returns whether a row changed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set(
        &self,
        tenant_id: &str,
        id: &str,
        name: &str,
        status: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE employees SET name = $3, status = $4, updated_at = now() \
                 WHERE tenant_id = $1 AND id = $2",
                &[&tenant_id, &id, &name, &status],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed == 1)
    }

    /// Sets (or resets) an employee's PIN to the given Argon2id PHC hash, within their tenant. Returns
    /// whether a row changed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_pin(
        &self,
        tenant_id: &str,
        id: &str,
        pin_phc: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE employees SET pin_phc = $3, updated_at = now() \
                 WHERE tenant_id = $1 AND id = $2",
                &[&tenant_id, &id, &pin_phc],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed == 1)
    }

    /// The stored Argon2id PHC hash of an employee's PIN, or `None` if unknown or unset. For the
    /// trusted publish path and tests only.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn pin_phc(&self, tenant_id: &str, id: &str) -> Result<Option<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                "SELECT pin_phc FROM employees WHERE tenant_id = $1 AND id = $2",
                &[&tenant_id, &id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.and_then(|row| row.get::<_, Option<String>>(0)))
    }
}

/// Reads one queried row into an [`EmployeeRow`]. The column order matches [`EMPLOYEE_COLUMNS`].
fn employee_row(row: &tokio_postgres::Row) -> EmployeeRow {
    EmployeeRow {
        id: row.get(0),
        tenant_id: row.get(1),
        code: row.get(2),
        name: row.get(3),
        status: row.get(4),
        has_pin: row.get(5),
    }
}

/// A role template as listed — identity, name, its permission-id set (as the JSON text stored in the
/// `jsonb` column), and status ([ADR-0070](../../../docs/adr/0070-people-and-access.md)).
#[derive(Clone, Debug)]
pub struct RoleTemplateRow {
    /// The role-template id (a ULID string).
    pub id: String,
    /// The owning tenant.
    pub tenant_id: String,
    /// The role name, unique within the tenant.
    pub name: String,
    /// The granted permission ids, as the JSON array text stored in the `jsonb` column.
    pub permissions_json: String,
    /// `active` or `archived`.
    pub status: String,
}

/// The role-template columns a read returns; `permissions` is read as its `jsonb` text.
const ROLE_TEMPLATE_COLUMNS: &str = "id, tenant_id, name, permissions::text, status";

/// An assignment as listed — identity plus the three ids it binds.
#[derive(Clone, Debug)]
pub struct AssignmentRow {
    /// The assignment id (a ULID string).
    pub id: String,
    /// The owning tenant.
    pub tenant_id: String,
    /// The assigned employee.
    pub employee_id: String,
    /// The store.
    pub store_id: String,
    /// The role that store grants.
    pub role_template_id: String,
}

/// The assignment columns a read returns.
const ASSIGNMENT_COLUMNS: &str = "id, tenant_id, employee_id, store_id, role_template_id";

impl PostgresPeople {
    /// Inserts a role template, its permission set given as JSON array text cast into the `jsonb`
    /// column.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails (including a
    /// duplicate `(tenant_id, name)`).
    pub async fn insert_role_template(
        &self,
        id: &str,
        tenant_id: &str,
        name: &str,
        permissions_json: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO role_templates (id, tenant_id, name, permissions) \
                 VALUES ($1, $2, $3, $4::text::jsonb)",
                &[&id, &tenant_id, &name, &permissions_json],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a tenant's role templates, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_role_templates(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<RoleTemplateRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {ROLE_TEMPLATE_COLUMNS} FROM role_templates \
                     WHERE tenant_id = $1 ORDER BY created_at DESC"
                ),
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(role_template_row).collect())
    }

    /// Reads one role template within its tenant, or `None`.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_role_template(
        &self,
        tenant_id: &str,
        id: &str,
    ) -> Result<Option<RoleTemplateRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_opt(
                &format!(
                    "SELECT {ROLE_TEMPLATE_COLUMNS} FROM role_templates \
                     WHERE tenant_id = $1 AND id = $2"
                ),
                &[&tenant_id, &id],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.as_ref().map(role_template_row))
    }

    /// Updates a role template's name, permission set, and status. Returns whether a row changed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_role_template(
        &self,
        tenant_id: &str,
        id: &str,
        name: &str,
        permissions_json: &str,
        status: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE role_templates \
                 SET name = $3, permissions = $4::text::jsonb, status = $5, updated_at = now() \
                 WHERE tenant_id = $1 AND id = $2",
                &[&tenant_id, &id, &name, &permissions_json, &status],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed == 1)
    }

    /// Inserts an assignment binding an employee to a store with a role.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails (including the
    /// same employee already assigned to that store).
    pub async fn insert_assignment(
        &self,
        id: &str,
        tenant_id: &str,
        employee_id: &str,
        store_id: &str,
        role_template_id: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO employee_store_assignments \
                 (id, tenant_id, employee_id, store_id, role_template_id) \
                 VALUES ($1, $2, $3, $4, $5)",
                &[&id, &tenant_id, &employee_id, &store_id, &role_template_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists the assignments at a store, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_assignments_for_store(
        &self,
        tenant_id: &str,
        store_id: &str,
    ) -> Result<Vec<AssignmentRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {ASSIGNMENT_COLUMNS} FROM employee_store_assignments \
                     WHERE tenant_id = $1 AND store_id = $2 ORDER BY created_at DESC"
                ),
                &[&tenant_id, &store_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(assignment_row).collect())
    }

    /// Lists the stores a person is assigned to, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_assignments_for_employee(
        &self,
        tenant_id: &str,
        employee_id: &str,
    ) -> Result<Vec<AssignmentRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {ASSIGNMENT_COLUMNS} FROM employee_store_assignments \
                     WHERE tenant_id = $1 AND employee_id = $2 ORDER BY created_at DESC"
                ),
                &[&tenant_id, &employee_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(assignment_row).collect())
    }

    /// Removes an assignment within its tenant. Returns whether a row was removed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn delete_assignment(&self, tenant_id: &str, id: &str) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let removed = connection
            .execute(
                "DELETE FROM employee_store_assignments WHERE tenant_id = $1 AND id = $2",
                &[&tenant_id, &id],
            )
            .await
            .map_err(unavailable)?;
        Ok(removed == 1)
    }
}

/// Reads one queried row into a [`RoleTemplateRow`]. The column order matches [`ROLE_TEMPLATE_COLUMNS`].
fn role_template_row(row: &tokio_postgres::Row) -> RoleTemplateRow {
    RoleTemplateRow {
        id: row.get(0),
        tenant_id: row.get(1),
        name: row.get(2),
        permissions_json: row.get(3),
        status: row.get(4),
    }
}

/// Reads one queried row into an [`AssignmentRow`]. The column order matches [`ASSIGNMENT_COLUMNS`].
fn assignment_row(row: &tokio_postgres::Row) -> AssignmentRow {
    AssignmentRow {
        id: row.get(0),
        tenant_id: row.get(1),
        employee_id: row.get(2),
        store_id: row.get(3),
        role_template_id: row.get(4),
    }
}
