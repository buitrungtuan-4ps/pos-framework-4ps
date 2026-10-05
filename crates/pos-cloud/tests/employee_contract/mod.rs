// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The [`EmployeeStore`] contract's rule for staff codes, held by every implementation of the seam:
//! the in-memory fake in `tests/cloud.rs`, and `PostgresPeople` against a real database in
//! `tests/employee_store_postgres.rs` (behind the `integration` feature).
//!
//! A staff code is unique within a tenant and free across tenants. A second employee with a code the
//! tenant already uses is refused as [`EmployeeStoreError::CodeInUse`], a conflict and not a failure
//! of the store, and writes nothing, so the code stays the first person's. The refusal never carries
//! the code: a staff code identifies a person.

use pos_cloud::people::{EmployeeId, EmployeeStore, EmployeeStoreError, NewEmployee};
use pos_proto::ids::TenantId;
use pos_proto::ulid::Ulid;

/// The staff code every hire below asks for.
const CODE: &str = "Q42";

/// Runs the rule against `employees`. Every id is `seed` plus a small number, so a database that
/// keeps rows from an earlier run never sees one of them twice.
pub(crate) async fn holds<E>(employees: &E, seed: u128)
where
    E: EmployeeStore + Sync,
{
    let id = |n: u128| Ulid::from_u128(seed + n);
    let (mine, other) = (TenantId::new(id(1)), TenantId::new(id(2)));
    let hire = |n: u128, tenant_id: TenantId, name: &str| NewEmployee {
        employee_id: EmployeeId::new(id(0x300 + n)),
        tenant_id,
        code: CODE.to_owned(),
        name: name.to_owned(),
    };

    employees
        .create(&hire(1, mine, "Alice"))
        .await
        .expect("the first employee with the code");
    let refused = employees.create(&hire(2, mine, "Bao")).await;
    assert!(
        matches!(refused, Err(EmployeeStoreError::CodeInUse)),
        "a second employee with the code in the same tenant is a duplicate: {refused:?}"
    );
    assert!(
        !format!("{refused:?}").contains(CODE),
        "the refusal never carries the code: {refused:?}"
    );
    let roster = employees.list(mine).await.expect("the tenant's roster");
    assert_eq!(
        roster
            .iter()
            .map(|row| (row.record.employee_id, row.record.name.as_str()))
            .collect::<Vec<_>>(),
        vec![(EmployeeId::new(id(0x301)), "Alice")],
        "the refused one wrote nothing, and the code is still the first person's"
    );

    employees
        .create(&hire(3, other, "Cam"))
        .await
        .expect("the same code in another tenant");
}
