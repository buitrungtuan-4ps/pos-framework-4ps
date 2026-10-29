// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The translation grid: one jsonb row per tenant, versioned by its own xmin (ADR-0095).

use store_postgres::RowUpdate;

use super::{TENANT_A, TENANT_B, block_on, prepared};

/// The grid saves under its version and refuses a save made against a version it has moved past.
///
/// The counterpart to the tax-rate case above, and the reason that one needed a migration and
/// this one did not: the grid is *one row*, so a save updates it in place and its own `xmin`
/// moves. ADR-0095 had booked both as needing a version table; measuring the schema showed only
/// one does.
#[test]
fn the_grid_saves_under_its_version_and_refuses_a_stale_one() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let grids = store.translations();
        let first = r#"{"menu.pho":{"en":"Pho"}}"#;
        let second = r#"{"menu.pho":{"en":"Pho noodles"}}"#;
        let clobber = r#"{"menu.pho":{"en":"Clobbered"}}"#;

        assert!(
            grids
                .load_grid(TENANT_A)
                .await
                .expect("load before any save")
                .is_none(),
            "no row before the first save"
        );

        let created = match grids
            .save_grid(TENANT_A, first, None)
            .await
            .expect("the create")
        {
            RowUpdate::Updated(version) => version,
            other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                panic!("expected the create to apply, got {other:?}")
            }
        };
        let (loaded, version) = grids
            .load_grid(TENANT_A)
            .await
            .expect("load")
            .expect("present");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&loaded).expect("valid json"),
            serde_json::from_str::<serde_json::Value>(first).expect("valid json"),
            "the grid round-trips (compared as JSON, since jsonb reorders keys)"
        );
        assert_eq!(version, created);

        let moved = match grids
            .save_grid(TENANT_A, second, Some(&version))
            .await
            .expect("the replace")
        {
            RowUpdate::Updated(version) => version,
            other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                panic!("expected the replace to apply, got {other:?}")
            }
        };
        assert_ne!(moved, version, "xmin must move on every UPDATE");

        // Replaying the old version, a garbled tag, and a false "nothing here yet" are all
        // refused; a version-gated save against a tenant with no row at all is a NotFound.
        for (expected, outcome) in [
            (Some(version.as_str()), RowUpdate::VersionMismatch),
            (Some("not-a-transaction-id"), RowUpdate::VersionMismatch),
            (None, RowUpdate::VersionMismatch),
        ] {
            assert_eq!(
                grids
                    .save_grid(TENANT_A, clobber, expected)
                    .await
                    .expect("the comparison must not raise"),
                outcome
            );
        }
        assert_eq!(
            grids
                .save_grid(TENANT_B, clobber, Some(&version))
                .await
                .expect("the probe"),
            RowUpdate::NotFound
        );

        let (survived, _) = grids
            .load_grid(TENANT_A)
            .await
            .expect("load")
            .expect("present");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&survived).expect("valid json"),
            serde_json::from_str::<serde_json::Value>(second).expect("valid json"),
            "no refused save changed the stored grid"
        );
    });
}
