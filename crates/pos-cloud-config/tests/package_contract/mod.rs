// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What every `PackageStore` holds to (ADR-0189 decision 1), run against the in-memory store by
//! `tests/packages.rs` and against PostgreSQL by `tests/packages_postgres.rs`.
//!
//! `seed` is the high half of every id the run writes, so a database that keeps an earlier run's
//! rows is read only for this run's.

use pos_cloud_api::version::CreateOutcome;
use pos_cloud_config::packages::{PackageRecord, PackageStore, PackageVersionRecord};
use pos_proto::ids::{PackageId, PackageVersionId, TenantId};
use pos_proto::package::PackageContent;
use pos_proto::text::{DisplayName, ReleaseTag};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;

fn at(milliseconds: i64) -> Timestamp {
    Timestamp::from_milliseconds_since_epoch(milliseconds).expect("a valid instant")
}

fn package(seed: u128, n: u128) -> PackageRecord {
    PackageRecord {
        package_id: PackageId::new(Ulid::from_u128(seed | n)),
        name: DisplayName::new("Counter screens"),
        create_time: at(1_000),
    }
}

fn version(seed: u128, package: &PackageRecord, n: u128, label: &str) -> PackageVersionRecord {
    PackageVersionRecord {
        content: PackageContent {
            package_id: package.package_id,
            package_version_id: PackageVersionId::new(Ulid::from_u128(seed | n)),
            version_label: label.to_owned(),
            min_edge_version: ReleaseTag::new("0.14.1"),
            time_slots: Vec::new(),
            flows: Vec::new(),
        },
        create_time: at(2_000),
    }
}

const fn created(outcome: &CreateOutcome) -> bool {
    matches!(outcome, CreateOutcome::Created(_))
}

/// Packages and versions are stored and read back whole, per tenant; a taken id is refused and the
/// row it names is left as it was, so a published version never changes.
pub(crate) async fn holds<S: PackageStore>(store: &S, seed: u128) {
    let tenant = TenantId::new(Ulid::from_u128(seed | 1));
    let other_tenant = TenantId::new(Ulid::from_u128(seed | 2));
    let first = package(seed, 10);
    let second = package(seed, 11);

    assert_eq!(store.list_packages(tenant).await.expect("list"), Vec::new());
    assert!(created(
        &store.create_package(tenant, &second).await.expect("create")
    ));
    assert!(created(
        &store.create_package(tenant, &first).await.expect("create")
    ));
    let mut renamed = first.clone();
    renamed.name = DisplayName::new("Something else");
    assert_eq!(
        store
            .create_package(tenant, &renamed)
            .await
            .expect("create again"),
        CreateOutcome::AlreadyExists
    );
    assert_eq!(
        store.list_packages(tenant).await.expect("list"),
        vec![first.clone(), second.clone()],
        "in id order, and the refused create changed nothing"
    );
    assert_eq!(
        store
            .get_package(tenant, first.package_id)
            .await
            .expect("get"),
        Some(first.clone())
    );
    assert_eq!(
        store.list_packages(other_tenant).await.expect("list"),
        Vec::new()
    );
    assert_eq!(
        store
            .get_package(other_tenant, first.package_id)
            .await
            .expect("get"),
        None,
        "another tenant does not see it"
    );

    let march = version(seed, &first, 21, "March");
    let april = version(seed, &first, 22, "April");
    let elsewhere = version(seed, &second, 23, "Bar");
    for record in [&april, &march, &elsewhere] {
        assert!(created(
            &store
                .publish_version(tenant, record)
                .await
                .expect("publish")
        ));
    }
    let mut rewritten = march.clone();
    "Rewritten".clone_into(&mut rewritten.content.version_label);
    assert_eq!(
        store
            .publish_version(tenant, &rewritten)
            .await
            .expect("publish again"),
        CreateOutcome::AlreadyExists
    );
    assert_eq!(
        store
            .list_versions(tenant, first.package_id)
            .await
            .expect("versions"),
        vec![march.clone(), april],
        "one package's versions, in id order, the refused publish changing nothing"
    );
    assert_eq!(
        store
            .get_version(tenant, march.content.package_version_id)
            .await
            .expect("get version"),
        Some(march.clone())
    );
    assert_eq!(
        store
            .get_version(other_tenant, march.content.package_version_id)
            .await
            .expect("get version"),
        None
    );
    assert_eq!(
        store
            .list_versions(other_tenant, first.package_id)
            .await
            .expect("versions"),
        Vec::new()
    );
}
