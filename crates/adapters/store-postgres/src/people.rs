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

use crate::store::{RowUpdate, pool_unavailable, unavailable, window_total};

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
    /// The version the row was read at, for a conditional write
    /// ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)). Opaque: this is
    /// `xmin::text`, and nothing above this crate may assume that.
    pub version: String,
}

/// The columns a read returns, in a stable order matching [`employee_row`]. `pin_phc` is deliberately
/// absent — reads expose only `pin_phc IS NOT NULL AS has_pin`, never the hash.
const EMPLOYEE_COLUMNS: &str =
    "id, tenant_id, code, name, status, (pin_phc IS NOT NULL) AS has_pin, xmin::text";

/// The order every employee read imposes, newest first — and *total*, because it ends in `id`, the
/// table's primary key.
///
/// The tiebreaker is what the paged read needs and what this order was missing. PostgreSQL's `now()`
/// is **transaction** time, so every row one transaction inserts carries the identical `created_at`
/// ([ADR-0098](../../../docs/adr/0098-paged-admin-reads.md) decision 9): a CSV import of a hundred
/// staff gives a hundred rows one instant, `created_at DESC` alone does not order them, and a
/// `LIMIT`/`OFFSET` window over a non-total order can return a row on two pages or on neither.
///
/// One const rather than a literal per query, so the unpaged read and the page cannot drift onto
/// different sequences — which would make "page 2" name rows from an order nothing else uses.
const EMPLOYEE_ORDER: &str = "ORDER BY created_at DESC, id DESC";

/// The paged read's optional filter: a case-insensitive substring of the person's **name or staff
/// code**, or everything when the parameter is `NULL`.
///
/// Both columns, because those are the two handles an operator has on someone they are looking for:
/// a name they were told, or the code on a badge. Neither alone would answer half the searches an
/// assign picker gets.
///
/// **Not `pin_phc`.** It is not selected by any read, and matching a substring against an Argon2id
/// hash would be meaningless even if it were — the only thing such a predicate could do is leak
/// timing about a secret.
///
/// `position(lower($n) in lower(col))` rather than `ILIKE`, so `%` and `_` in what the operator
/// typed are characters and not wildcards — the same choice, for the same reason, as the catalog
/// item search. It cannot use `employees_code_key`: a substring match is not a prefix match. That is
/// acceptable here in a way it would not be on the event tables, because the predicate runs over one
/// tenant's employees — hundreds of rows that `employees_by_tenant_newest` has already narrowed to —
/// not over a table that grows with traffic.
const EMPLOYEE_SEARCH: &str = "($2::text IS NULL \
       OR position(lower($2) in lower(name)) > 0 \
       OR position(lower($2) in lower(code)) > 0)";

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
    ) -> Result<String, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_one(
                "INSERT INTO employees (id, tenant_id, code, name) VALUES ($1, $2, $3, $4) \
                 RETURNING xmin::text",
                &[&id, &tenant_id, &code, &name],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.get(0))
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
                     WHERE tenant_id = $1 {EMPLOYEE_ORDER}"
                ),
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(employee_row).collect())
    }

    /// One page of a tenant's employees matching `search`, newest first, with how many matched.
    ///
    /// The same columns and the same order as [`fetch`](Self::fetch), so a page is a window onto the
    /// sequence that read returns — including the same deliberate absence of `pin_phc`. Paging
    /// changes how much of a tenant's roster crosses the wire at once; it changes no field
    /// ([ADR-0070](../../../docs/adr/0070-people-and-access.md) is what decides which fields a read
    /// exposes, and this does not touch it).
    ///
    /// `count(*) OVER()` rides on the windowed `SELECT`: one round trip, one snapshot, so the count
    /// cannot disagree with the page it labels. `employees_by_tenant_newest` (migration 0045) carries
    /// the whole order, so neither the window nor the count needs a sort above the scan.
    ///
    /// The one case that needs a second statement is an *empty* window — a page past the end, or a
    /// roster that shrank under a pager sitting on page four. `count(*) OVER()` has no row to ride
    /// on there, and reporting `0` would tell the caller the tenant has no staff. [`window_total`]
    /// is where that case is handled, for this read and every other paged read in the adapter.
    ///
    /// `search` narrows on [`EMPLOYEE_SEARCH`], and the total narrows with it: the count answers
    /// "how many matched", not "how big is the roster", because that is the number a pager over the
    /// results has to size itself from.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_page(
        &self,
        tenant_id: &str,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<EmployeeRow>, i64), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                &format!(
                    "SELECT {EMPLOYEE_COLUMNS}, count(*) OVER() FROM employees \
                     WHERE tenant_id = $1 AND {EMPLOYEE_SEARCH} \
                     {EMPLOYEE_ORDER} LIMIT $3 OFFSET $4"
                ),
                &[&tenant_id, &search, &limit, &offset],
            )
            .await
            .map_err(unavailable)?;
        // Every row carries the same headcount — but an empty window carries no row to read it off,
        // and `0` is then a wrong answer rather than a missing one: it says the tenant has nobody,
        // when what happened is that the caller asked past the end of a roster that does have
        // people. The pager on the other side reads this number to decide how many pages exist, so
        // it gets counted properly. One extra round trip, only on the path that returned nothing.
        let total = window_total(
            &connection,
            &rows,
            7,
            &format!("SELECT count(*) FROM employees WHERE tenant_id = $1 AND {EMPLOYEE_SEARCH}"),
            &[&tenant_id, &search],
        )
        .await?;
        Ok((rows.iter().map(employee_row).collect(), total))
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

    /// Renames an employee and sets their status, within their tenant. Applies only if the row is still at `expected`.
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
        expected: &str,
    ) -> Result<RowUpdate, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let updated = connection
            .query_opt(
                "UPDATE employees SET name = $3, status = $4, updated_at = now() \
                 WHERE tenant_id = $1 AND id = $2 \
                 AND xmin::text = $5 RETURNING xmin::text",
                &[&tenant_id, &id, &name, &status, &expected],
            )
            .await
            .map_err(unavailable)?;
        if let Some(row) = updated {
            return Ok(RowUpdate::Updated(row.get(0)));
        }
        let present = connection
            .query_opt(
                "SELECT 1 FROM employees WHERE tenant_id = $1 AND id = $2",
                &[&tenant_id, &id],
            )
            .await
            .map_err(unavailable)?;
        Ok(if present.is_some() {
            RowUpdate::VersionMismatch
        } else {
            RowUpdate::NotFound
        })
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
        version: row.get(6),
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
    /// The version the row was read at, for a conditional write
    /// ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)). Opaque: this is
    /// `xmin::text`, and nothing above this crate may assume that.
    pub version: String,
}

/// The role-template columns a read returns; `permissions` is read as its `jsonb` text.
const ROLE_TEMPLATE_COLUMNS: &str = "id, tenant_id, name, permissions::text, status, xmin::text";

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
    /// The assigned person's name, resolved by the join. `None` only if no employee row matches.
    pub employee_name: Option<String>,
    /// The assigned person's staff code, resolved by the join. `None` on the same terms as the name.
    pub employee_code: Option<String>,
}

/// The assignment columns a read returns, qualified for [`ASSIGNMENT_JOIN`] and in
/// [`assignment_row`]'s order. `created_at` is qualified at every call site for the same reason the
/// ids are: both joined tables carry a column of that name.
const ASSIGNMENT_COLUMNS: &str = "a.id, a.tenant_id, a.employee_id, a.store_id, a.role_template_id, \
     e.name, e.code";

/// The `FROM` every assignment read uses: the grants, with the assigned person joined in.
///
/// Resolving the name here rather than in the caller is what lets the console show who an assignment
/// belongs to without reading the tenant's whole roster to look the id up — the read that stops
/// working once the roster is paged
/// ([ADR-0098](../../../docs/adr/0098-paged-admin-reads.md), B3-4). A store's assignments are tens of
/// rows and the join is on the employees' primary key, so this costs an index lookup per row and no
/// second query.
///
/// **`LEFT`, not `INNER`.** `employee_store_assignments` declares no foreign key to `employees`
/// (migration 0024 has none, and nothing since adds one), so nothing in the schema stops an
/// assignment outliving the row it points at. An inner join would drop such a row from the list —
/// turning a data problem into an invisible one, and quietly removing a grant the operator can still
/// see the effects of. A left join surfaces it with no name, which the console renders as the raw id,
/// exactly what it did before this read resolved anything.
///
/// **Personal data (T1).** `name` and `code` are the employee record's, so this read now returns
/// personal data where it used to return only ids
/// ([ADR-0070](../../../docs/adr/0070-people-and-access.md)). That is a redistribution, not an
/// expansion: the caller reaches this behind `console.people.manage`, the same gate that lets them
/// read the roster itself, and the console screen already displayed the name — by fetching the whole
/// roster to find it. Nothing new is exposed, to nobody new, and `pin_phc` is not selected here any
/// more than it is anywhere else.
const ASSIGNMENT_JOIN: &str = "employee_store_assignments a \
     LEFT JOIN employees e ON e.id = a.employee_id AND e.tenant_id = a.tenant_id";

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
    ) -> Result<String, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let row = connection
            .query_one(
                "INSERT INTO role_templates (id, tenant_id, name, permissions) \
                 VALUES ($1, $2, $3, $4::text::jsonb) \
                 RETURNING xmin::text",
                &[&id, &tenant_id, &name, &permissions_json],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.get(0))
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

    /// Updates a role template's name, permission set, and status. Applies only if the row is still at `expected`.
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
        expected: &str,
    ) -> Result<RowUpdate, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let updated = connection
            .query_opt(
                "UPDATE role_templates \
                 SET name = $3, permissions = $4::text::jsonb, status = $5, updated_at = now() \
                 WHERE tenant_id = $1 AND id = $2 \
                 AND xmin::text = $6 RETURNING xmin::text",
                &[
                    &tenant_id,
                    &id,
                    &name,
                    &permissions_json,
                    &status,
                    &expected,
                ],
            )
            .await
            .map_err(unavailable)?;
        if let Some(row) = updated {
            return Ok(RowUpdate::Updated(row.get(0)));
        }
        let present = connection
            .query_opt(
                "SELECT 1 FROM role_templates WHERE tenant_id = $1 AND id = $2",
                &[&tenant_id, &id],
            )
            .await
            .map_err(unavailable)?;
        Ok(if present.is_some() {
            RowUpdate::VersionMismatch
        } else {
            RowUpdate::NotFound
        })
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
                    "SELECT {ASSIGNMENT_COLUMNS} FROM {ASSIGNMENT_JOIN} \
                     WHERE a.tenant_id = $1 AND a.store_id = $2 ORDER BY a.created_at DESC"
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
                    "SELECT {ASSIGNMENT_COLUMNS} FROM {ASSIGNMENT_JOIN} \
                     WHERE a.tenant_id = $1 AND a.employee_id = $2 ORDER BY a.created_at DESC"
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
        version: row.get(5),
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
        employee_name: row.get(5),
        employee_code: row.get(6),
    }
}

#[cfg(test)]
mod tests {
    use super::{EMPLOYEE_COLUMNS, EMPLOYEE_ORDER};

    /// The roster order is *total*, because it ends in the primary key.
    ///
    /// This test exists because nothing else in the tree can see it. The integration suite's
    /// `EXPLAIN` guard asserts the plan of a query *it* writes, so it catches migration 0045 being
    /// dropped — but if this adapter's `ORDER BY` lost its tiebreaker the guard would keep passing,
    /// and so would the page-partition tests: with the index present, the index walk supplies the
    /// missing order for free. The same blind spot was found by mutation on the catalog fragments.
    #[test]
    fn the_roster_order_ends_in_the_primary_key_so_a_window_over_it_is_unambiguous() {
        assert!(
            EMPLOYEE_ORDER.ends_with("id DESC"),
            "the roster order must break ties on the primary key, or LIMIT/OFFSET over it can \
             repeat or skip a row when a transaction inserted several staff at once; got \
             {EMPLOYEE_ORDER:?}",
        );
        assert!(
            EMPLOYEE_ORDER.contains("created_at DESC"),
            "newest-first is still the order the roster is read in; got {EMPLOYEE_ORDER:?}",
        );
    }

    /// The read exposes `has_pin`, never the PIN hash.
    ///
    /// [ADR-0070](../../../docs/adr/0070-people-and-access.md)'s rule, asserted here rather than
    /// trusted to a docstring: `pin_phc` reaches the API through no read, and the paged read added
    /// beside `fetch` shares this one column list precisely so it cannot acquire a field of its own.
    #[test]
    fn no_read_exposes_the_pin_hash() {
        assert!(
            EMPLOYEE_COLUMNS.contains("(pin_phc IS NOT NULL) AS has_pin"),
            "a read tells a caller whether a PIN is set; got {EMPLOYEE_COLUMNS:?}",
        );
        assert!(
            !EMPLOYEE_COLUMNS.contains("pin_phc,") && !EMPLOYEE_COLUMNS.ends_with("pin_phc"),
            "no read selects the hash itself; got {EMPLOYEE_COLUMNS:?}",
        );
    }
}
