// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! People's persistence seams over their `store-postgres` tables: the `EmployeeStore`,
//! `RoleTemplateStore`, `AssignmentStore` and `PeopleRepublishStore` impls, moved out of
//! `pos-cloud`'s `persistence.rs` with their traits. A trait can be implemented for another crate's
//! type only in the trait's crate, so the glue lives here; all SQL stays in the adapter.

use store_postgres::{
    AssignmentRow, EmployeeOrder, EmployeeRow, PeopleRepublishRow, PostgresPeople,
    PostgresPeopleRepublishes, RoleTemplateRow,
};

use pos_ports::PortError;
use pos_proto::ErrorStatus;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;

use crate::paging::{Page, PageRequest};
use crate::people::{
    Assignment, AssignmentId, AssignmentScope, AssignmentStore, AssignmentStoreError, Employee,
    EmployeeId, EmployeeListFilter, EmployeeSort, EmployeeStore, EmployeeStoreError,
    EmployeeUpdate, NewAssignment, NewEmployee, NewRoleTemplate, RoleTemplate, RoleTemplateId,
    RoleTemplateStore, RoleTemplateStoreError, RoleTemplateUpdate,
};
use crate::people_republish::{PeopleRepublish, PeopleRepublishError, PeopleRepublishStore};
use crate::registry::EntityStatus;
use crate::store_groups::StoreGroupId;
use crate::version::{UpdateOutcome, Version, Versioned};
use pos_cloud_config::persistence::update_outcome;

// --- people & access (Track M1, ADR-0070): the `employees` rows converted to the EmployeeStore domain ---

/// Converts a stored employee row into the domain [`Employee`], parsing the ids and status. A row with
/// an unparseable id is corruption the caller should see, not silently drop.
fn employee_record(row: EmployeeRow) -> Result<Versioned<Employee>, EmployeeStoreError> {
    let employee_id = row
        .id
        .parse::<Ulid>()
        .map(EmployeeId::new)
        .map_err(|error| {
            EmployeeStoreError::new(format!("stored employee id is not a ULID: {error}"))
        })?;
    let tenant_id = row
        .tenant_id
        .parse::<Ulid>()
        .map(TenantId::new)
        .map_err(|error| {
            EmployeeStoreError::new(format!("stored tenant id is not a ULID: {error}"))
        })?;
    let version = Version::new(row.version);
    Ok(Versioned::new(
        Employee {
            employee_id,
            tenant_id,
            code: row.code,
            name: row.name,
            status: EntityStatus::from_db(&row.status),
            has_pin: row.has_pin,
        },
        version,
    ))
}

impl EmployeeStore for PostgresPeople {
    async fn create(&self, employee: &NewEmployee) -> Result<Version, EmployeeStoreError> {
        self.insert(
            &employee.employee_id.to_string(),
            &employee.tenant_id.to_string(),
            &employee.code,
            &employee.name,
        )
        .await
        .map(Version::new)
        .map_err(|error| {
            // The adapter reports the unique index refusing a second code as `already_exists`: a
            // conflict the operator resolves by choosing another code, not an outage.
            if error.status() == ErrorStatus::AlreadyExists {
                EmployeeStoreError::CodeInUse
            } else {
                EmployeeStoreError::new(error.to_string())
            }
        })
    }

    async fn list(&self, tenant: TenantId) -> Result<Vec<Versioned<Employee>>, EmployeeStoreError> {
        let rows = self
            .fetch(&tenant.to_string())
            .await
            .map_err(|error| EmployeeStoreError::new(error.to_string()))?;
        rows.into_iter().map(employee_record).collect()
    }

    async fn list_page(
        &self,
        tenant: TenantId,
        page: PageRequest,
        filter: &EmployeeListFilter,
    ) -> Result<Page<Versioned<Employee>>, EmployeeStoreError> {
        // The one place the wire's sort token becomes an `ORDER BY`. `wildcard_enum_match_arm` is
        // denied workspace-wide, so a new `EmployeeSort` variant fails here rather than quietly
        // ordering by `created_at`.
        let order = match filter.sort {
            EmployeeSort::Newest => EmployeeOrder::Newest,
            EmployeeSort::Name => EmployeeOrder::Name,
            EmployeeSort::Code => EmployeeOrder::Code,
        };
        let (rows, total) = self
            .fetch_page(
                &tenant.to_string(),
                filter.search.as_deref(),
                order,
                filter.descending,
                i64::from(page.limit()),
                i64::from(page.offset()),
            )
            .await
            .map_err(|error| EmployeeStoreError::new(error.to_string()))?;
        let employees: Vec<Versioned<Employee>> = rows
            .into_iter()
            .map(employee_record)
            .collect::<Result<_, _>>()?;
        Ok(Page::new(
            employees,
            u32::try_from(total).unwrap_or(u32::MAX),
        ))
    }

    async fn get(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> Result<Option<Versioned<Employee>>, EmployeeStoreError> {
        let row = self
            .fetch_one(&tenant.to_string(), &employee_id.to_string())
            .await
            .map_err(|error| EmployeeStoreError::new(error.to_string()))?;
        row.map(employee_record).transpose()
    }

    async fn update(
        &self,
        employee: &EmployeeUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, EmployeeStoreError> {
        self.set(
            &employee.tenant_id.to_string(),
            &employee.employee_id.to_string(),
            &employee.name,
            employee.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| EmployeeStoreError::new(error.to_string()))
    }

    async fn set_pin(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
        pin_phc: &str,
    ) -> Result<bool, EmployeeStoreError> {
        PostgresPeople::set_pin(self, &tenant.to_string(), &employee_id.to_string(), pin_phc)
            .await
            .map_err(|error| EmployeeStoreError::new(error.to_string()))
    }

    async fn pin_phc(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> Result<Option<String>, EmployeeStoreError> {
        PostgresPeople::pin_phc(self, &tenant.to_string(), &employee_id.to_string())
            .await
            .map_err(|error| EmployeeStoreError::new(error.to_string()))
    }
}

/// Converts a stored role-template row into the domain [`RoleTemplate`], parsing the ids, status, and
/// the two `jsonb` permission arrays.
fn role_template_record(
    row: RoleTemplateRow,
) -> Result<Versioned<RoleTemplate>, RoleTemplateStoreError> {
    let role_template_id = row
        .id
        .parse::<Ulid>()
        .map(RoleTemplateId::new)
        .map_err(|error| {
            RoleTemplateStoreError::new(format!("stored role-template id is not a ULID: {error}"))
        })?;
    let tenant_id = row
        .tenant_id
        .parse::<Ulid>()
        .map(TenantId::new)
        .map_err(|error| {
            RoleTemplateStoreError::new(format!("stored tenant id is not a ULID: {error}"))
        })?;
    let permissions: Vec<String> =
        serde_json::from_str(&row.permissions_json).map_err(|error| {
            RoleTemplateStoreError::new(format!(
                "stored role-template permissions are not JSON: {error}"
            ))
        })?;
    let permissions_with_approval: Vec<String> =
        serde_json::from_str(&row.permissions_with_approval_json).map_err(|error| {
            RoleTemplateStoreError::new(format!(
                "stored role-template permissions with approval are not JSON: {error}"
            ))
        })?;
    let version = Version::new(row.version);
    Ok(Versioned::new(
        RoleTemplate {
            role_template_id,
            tenant_id,
            name: row.name,
            permissions,
            permissions_with_approval,
            discount_ceiling_minor: row.discount_ceiling_minor,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

/// A list of permission ids as the JSON array text a role's `jsonb` columns take.
fn permission_ids_json(ids: &[String]) -> Result<String, RoleTemplateStoreError> {
    serde_json::to_string(ids).map_err(|error| {
        RoleTemplateStoreError::new(format!("cannot serialize permissions: {error}"))
    })
}

impl RoleTemplateStore for PostgresPeople {
    async fn create(&self, template: &NewRoleTemplate) -> Result<Version, RoleTemplateStoreError> {
        let permissions_json = permission_ids_json(&template.permissions)?;
        let permissions_with_approval_json =
            permission_ids_json(&template.permissions_with_approval)?;
        self.insert_role_template(
            &template.role_template_id.to_string(),
            &template.tenant_id.to_string(),
            &template.name,
            &permissions_json,
            &permissions_with_approval_json,
            template.discount_ceiling_minor,
        )
        .await
        .map(Version::new)
        .map_err(|error| RoleTemplateStoreError::new(error.to_string()))
    }

    async fn list(
        &self,
        tenant: TenantId,
    ) -> Result<Vec<Versioned<RoleTemplate>>, RoleTemplateStoreError> {
        let rows = self
            .fetch_role_templates(&tenant.to_string())
            .await
            .map_err(|error| RoleTemplateStoreError::new(error.to_string()))?;
        rows.into_iter().map(role_template_record).collect()
    }

    async fn get(
        &self,
        tenant: TenantId,
        role_template_id: RoleTemplateId,
    ) -> Result<Option<Versioned<RoleTemplate>>, RoleTemplateStoreError> {
        let row = self
            .fetch_role_template(&tenant.to_string(), &role_template_id.to_string())
            .await
            .map_err(|error| RoleTemplateStoreError::new(error.to_string()))?;
        row.map(role_template_record).transpose()
    }

    async fn update(
        &self,
        template: &RoleTemplateUpdate,
        expected: &Version,
    ) -> Result<UpdateOutcome, RoleTemplateStoreError> {
        let permissions_json = permission_ids_json(&template.permissions)?;
        let permissions_with_approval_json =
            permission_ids_json(&template.permissions_with_approval)?;
        self.set_role_template(
            &template.tenant_id.to_string(),
            &template.role_template_id.to_string(),
            &template.name,
            &permissions_json,
            &permissions_with_approval_json,
            template.status.as_str(),
            template.discount_ceiling_minor,
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| RoleTemplateStoreError::new(error.to_string()))
    }
}

/// Parses one stored assignment id, naming the field in the error for the server log.
fn parse_assignment_ulid(text: &str, what: &str) -> Result<Ulid, AssignmentStoreError> {
    text.parse::<Ulid>().map_err(|error| {
        AssignmentStoreError::new(format!("stored {what} id is not a ULID: {error}"))
    })
}

/// The scope a stored row names: `STORE` with its store, `STORE_GROUP` with its group, or `TENANT`
/// with neither (`employee_scope_assignments`, migration 0077).
fn stored_scope(row: &AssignmentRow) -> Result<AssignmentScope, AssignmentStoreError> {
    match (
        row.scope_kind.as_str(),
        row.store_id.as_deref(),
        row.store_group_id.as_deref(),
    ) {
        ("STORE", Some(store_id), None) => Ok(AssignmentScope::Store(StoreId::new(
            parse_assignment_ulid(store_id, "store")?,
        ))),
        ("STORE_GROUP", None, Some(store_group_id)) => Ok(AssignmentScope::StoreGroup(
            StoreGroupId::new(parse_assignment_ulid(store_group_id, "store-group")?),
        )),
        ("TENANT", None, None) => Ok(AssignmentScope::Tenant),
        (kind, _, _) => Err(AssignmentStoreError::new(format!(
            "stored assignment scope {kind} does not carry the id it needs"
        ))),
    }
}

/// Converts a stored assignment row into the domain [`Assignment`], parsing its ids and scope.
fn assignment_record(row: &AssignmentRow) -> Result<Assignment, AssignmentStoreError> {
    Ok(Assignment {
        assignment_id: AssignmentId::new(parse_assignment_ulid(&row.id, "assignment")?),
        tenant_id: TenantId::new(parse_assignment_ulid(&row.tenant_id, "tenant")?),
        employee_id: EmployeeId::new(parse_assignment_ulid(&row.employee_id, "employee")?),
        scope: stored_scope(row)?,
        role_template_id: RoleTemplateId::new(parse_assignment_ulid(
            &row.role_template_id,
            "role-template",
        )?),
        employee_name: row.employee_name.clone(),
        employee_code: row.employee_code.clone(),
    })
}

/// Converts a list of stored assignment rows, failing on the first that does not parse.
fn assignment_records(
    rows: Result<Vec<AssignmentRow>, PortError>,
) -> Result<Vec<Assignment>, AssignmentStoreError> {
    rows.map_err(|error| AssignmentStoreError::new(error.to_string()))?
        .iter()
        .map(assignment_record)
        .collect()
}

impl AssignmentStore for PostgresPeople {
    async fn assign(&self, assignment: &NewAssignment) -> Result<(), AssignmentStoreError> {
        let assignment_id = assignment.assignment_id.to_string();
        let tenant_id = assignment.tenant_id.to_string();
        let employee_id = assignment.employee_id.to_string();
        let role_template_id = assignment.role_template_id.to_string();
        // A one-store grant is a row of the table it always was, so a release without the wider
        // scopes still reads it; a wider one is a row of its own table (migration 0077).
        let written = match assignment.scope {
            AssignmentScope::Store(store_id) => {
                self.insert_assignment(
                    &assignment_id,
                    &tenant_id,
                    &employee_id,
                    &store_id.to_string(),
                    &role_template_id,
                )
                .await
            }
            AssignmentScope::StoreGroup(store_group_id) => {
                self.insert_scope_assignment(
                    &assignment_id,
                    &tenant_id,
                    &employee_id,
                    "STORE_GROUP",
                    Some(&store_group_id.to_string()),
                    &role_template_id,
                )
                .await
            }
            AssignmentScope::Tenant => {
                self.insert_scope_assignment(
                    &assignment_id,
                    &tenant_id,
                    &employee_id,
                    "TENANT",
                    None,
                    &role_template_id,
                )
                .await
            }
        };
        // The adapter reports the unique index refusing a second row as `already_exists`: the
        // person is assigned there already, which is a conflict and not an outage.
        written.map_err(|error| {
            if error.status() == ErrorStatus::AlreadyExists {
                AssignmentStoreError::AlreadyAssigned(error.message().to_owned())
            } else {
                AssignmentStoreError::new(error.to_string())
            }
        })
    }

    async fn list_for_store(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        assignment_records(
            self.fetch_assignments_for_store(&tenant.to_string(), &store_id.to_string())
                .await,
        )
    }

    async fn list_for_group(
        &self,
        tenant: TenantId,
        store_group_id: StoreGroupId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        assignment_records(
            self.fetch_assignments_for_group(&tenant.to_string(), &store_group_id.to_string())
                .await,
        )
    }

    async fn list_tenant_wide(
        &self,
        tenant: TenantId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        assignment_records(
            self.fetch_tenant_wide_assignments(&tenant.to_string())
                .await,
        )
    }

    async fn list_for_employee(
        &self,
        tenant: TenantId,
        employee_id: EmployeeId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        assignment_records(
            self.fetch_assignments_for_employee(&tenant.to_string(), &employee_id.to_string())
                .await,
        )
    }

    async fn list_for_role(
        &self,
        tenant: TenantId,
        role_template_id: RoleTemplateId,
    ) -> Result<Vec<Assignment>, AssignmentStoreError> {
        assignment_records(
            self.fetch_assignments_for_role(&tenant.to_string(), &role_template_id.to_string())
                .await,
        )
    }

    async fn get(
        &self,
        tenant: TenantId,
        assignment_id: AssignmentId,
    ) -> Result<Option<Assignment>, AssignmentStoreError> {
        let row = self
            .fetch_assignment(&tenant.to_string(), &assignment_id.to_string())
            .await
            .map_err(|error| AssignmentStoreError::new(error.to_string()))?;
        row.as_ref().map(assignment_record).transpose()
    }

    async fn remove(
        &self,
        tenant: TenantId,
        assignment_id: AssignmentId,
    ) -> Result<bool, AssignmentStoreError> {
        self.delete_assignment(&tenant.to_string(), &assignment_id.to_string())
            .await
            .map_err(|error| AssignmentStoreError::new(error.to_string()))
    }
}

/// Rehydrates a queued store, failing loudly on an id or a time this cloud wrote that no longer
/// parses — store corruption, not an absence — as a scheduled publish's row does.
fn republish_from_row(row: PeopleRepublishRow) -> Result<PeopleRepublish, PeopleRepublishError> {
    let tenant_id = row
        .tenant_id
        .parse::<Ulid>()
        .map(TenantId::new)
        .map_err(|_ignored| PeopleRepublishError::new("a queued store has a non-ULID tenant"))?;
    let store_id = row
        .store_id
        .parse::<Ulid>()
        .map(StoreId::new)
        .map_err(|_ignored| PeopleRepublishError::new("a queued store has a non-ULID id"))?;
    let enqueued_time = Timestamp::from_milliseconds_since_epoch(row.enqueued_time_ms)
        .map_err(|error| PeopleRepublishError::new(error.to_string()))?;
    Ok(PeopleRepublish {
        tenant_id,
        store_id,
        reason: row.reason,
        enqueued_time,
    })
}

impl PeopleRepublishStore for PostgresPeopleRepublishes {
    async fn enqueue(&self, republish: &PeopleRepublish) -> Result<(), PeopleRepublishError> {
        let row = PeopleRepublishRow {
            tenant_id: republish.tenant_id.to_string(),
            store_id: republish.store_id.to_string(),
            reason: republish.reason.clone(),
            enqueued_time_ms: republish.enqueued_time.as_milliseconds_since_epoch(),
        };
        Self::enqueue(self, &row)
            .await
            .map_err(|error| PeopleRepublishError::new(error.to_string()))
    }

    async fn pending(
        &self,
        after: Option<(TenantId, StoreId)>,
        limit: usize,
    ) -> Result<Vec<PeopleRepublish>, PeopleRepublishError> {
        let after =
            after.map(|(tenant_id, store_id)| (tenant_id.to_string(), store_id.to_string()));
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = Self::pending(
            self,
            after
                .as_ref()
                .map(|(tenant_id, store_id)| (tenant_id.as_str(), store_id.as_str())),
            limit,
        )
        .await
        .map_err(|error| PeopleRepublishError::new(error.to_string()))?;
        rows.into_iter().map(republish_from_row).collect()
    }

    async fn clear(&self, republish: &PeopleRepublish) -> Result<bool, PeopleRepublishError> {
        Self::clear(
            self,
            &republish.tenant_id.to_string(),
            &republish.store_id.to_string(),
            republish.enqueued_time.as_milliseconds_since_epoch(),
        )
        .await
        .map_err(|error| PeopleRepublishError::new(error.to_string()))
    }
}
