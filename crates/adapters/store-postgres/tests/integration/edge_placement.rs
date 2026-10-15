// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The lease bump against the real column, real default and real `CHECK`
//! ([ADR-0110](../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md), migration 0052).

use pos_proto::{StoreId, TenantId, Ulid};

use super::{block_on, prepared};

/// A fixed pair, so the assertions read as one store's history rather than as arithmetic.
fn ids() -> (TenantId, StoreId) {
    (
        TenantId::new(Ulid::from_u128(0x0EDA)),
        StoreId::new(Ulid::from_u128(0x0EDB)),
    )
}

#[test]
fn a_bump_sets_the_placement_and_a_later_one_without_it_keeps_it() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let trees = store.config_trees();
        let (tenant, store_id) = ids();

        // First bump, no placement named: generation 0, and the column falls to its schema
        // default rather than to the wire's UNSPECIFIED. This is the INSERT branch's COALESCE.
        let first = super::issue(
            &trees,
            tenant,
            store_id,
            1_777_000_000_000,
            None,
            None,
            None,
        )
        .await;
        assert_eq!(first.generation, 0);
        assert_eq!(first.edge_placement, "EDGE_PLACEMENT_IN_STORE");

        // Named: the store moves, and the generation advances in the same statement.
        let moved = super::issue(
            &trees,
            tenant,
            store_id,
            1_777_000_001_000,
            Some("EDGE_PLACEMENT_HOSTED_BY_PLATFORM"),
            None,
            Some(0),
        )
        .await;
        assert_eq!(moved.generation, 1);
        assert_eq!(moved.edge_placement, "EDGE_PLACEMENT_HOSTED_BY_PLATFORM");

        // Not named: ADR-0003's swap of the hosted machine. The ON CONFLICT branch's COALESCE
        // keeps the stored value. Drop that COALESCE and this returns IN_STORE — a store
        // silently re-promised offline trading it cannot do.
        //
        // It also acknowledges generation 0, which the move above left undrained. Without that
        // this bump is refused — which is the point of the refusal, and is asserted on its own
        // below.
        let swapped = super::issue(
            &trees,
            tenant,
            store_id,
            1_777_000_002_000,
            None,
            Some(0),
            Some(1),
        )
        .await;
        assert_eq!(swapped.generation, 2);
        assert_eq!(
            swapped.edge_placement, "EDGE_PLACEMENT_HOSTED_BY_PLATFORM",
            "a bump naming no placement must keep the one the store had"
        );
    });
}

/// The region rides the same statement as the placement it qualifies
/// ([ADR-0114](../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// Three behaviours, and none of them is provable by the unit tests or the in-process suite:
/// the `CASE WHEN` that lets a swap keep a region, the insert branch that writes one on a
/// store's first-ever lease, and the pair being cleared together when a store comes home.
#[test]
fn a_bump_writes_the_region_and_a_swap_keeps_it_and_coming_home_clears_it() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let trees = store.config_trees();
        let (tenant, store_id) = (
            TenantId::new(Ulid::from_u128(0x0EDC)),
            StoreId::new(Ulid::from_u128(0x0EDD)),
        );

        // A store's first-ever lease, hosted from the start: the INSERT branch carries the
        // pair. Written straight rather than through the `CASE`, because a row that does not
        // exist has no region to keep.
        let first = super::issue_with_region(
            &trees,
            tenant,
            store_id,
            1_777_000_000_000,
            Some("EDGE_PLACEMENT_HOSTED_BY_OPERATOR"),
            store_postgres::StoredRegionWrite::Set {
                country: "SG",
                label: "ap-southeast-1",
            },
            None,
            None,
        )
        .await;
        assert_eq!(first.generation, 0);
        assert_eq!(first.region_country.as_deref(), Some("SG"));
        assert_eq!(first.region_label.as_deref(), Some("ap-southeast-1"));

        // A swap in place: the machine is replaced and the place is not. `CASE WHEN $7` is
        // false, so both columns are left alone. Drop that `CASE` for a plain assignment and
        // this returns NULL — a hosted store with no recorded region, which is the exact state
        // ADR-0114 exists to prevent.
        let swapped = super::issue(
            &trees,
            tenant,
            store_id,
            1_777_000_001_000,
            None,
            None,
            Some(0),
        )
        .await;
        assert_eq!(swapped.generation, 1);
        assert_eq!(
            swapped.region_country.as_deref(),
            Some("SG"),
            "a swap in place keeps the region, exactly as it keeps the placement"
        );
        assert_eq!(swapped.region_label.as_deref(), Some("ap-southeast-1"));

        // Moved again, to a different place. The `CASE` writes both.
        let moved = super::issue_with_region(
            &trees,
            tenant,
            store_id,
            1_777_000_002_000,
            Some("EDGE_PLACEMENT_HOSTED_BY_PLATFORM"),
            store_postgres::StoredRegionWrite::Set {
                country: "JP",
                label: "堺のラック",
            },
            Some(0),
            Some(1),
        )
        .await;
        assert_eq!(moved.region_country.as_deref(), Some("JP"));
        assert_eq!(
            moved.region_label.as_deref(),
            Some("堺のラック"),
            "the label is `text`, so a non-ASCII place name round-trips unchanged"
        );

        // Home to the shop. Both columns go to NULL in the same `SET` clause that writes the
        // placement, so there is no instant at which the row says in-store *and* names a rack
        // in Sakai.
        let home = super::issue_with_region(
            &trees,
            tenant,
            store_id,
            1_777_000_003_000,
            Some("EDGE_PLACEMENT_IN_STORE"),
            store_postgres::StoredRegionWrite::Clear,
            Some(1),
            Some(2),
        )
        .await;
        assert_eq!(home.edge_placement, "EDGE_PLACEMENT_IN_STORE");
        assert!(
            home.region_country.is_none() && home.region_label.is_none(),
            "an in-store machine is in the shop and has no region"
        );
    });
}

#[test]
fn the_database_refuses_a_placement_the_framework_never_defined() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let trees = store.config_trees();
        let (tenant, store_id) = ids();

        // The route already refuses this, so the CHECK is the second line: it catches a token
        // that reached the column some other way — a hand-run UPDATE during an incident, a
        // fork's own writer. Without it the console would render UNSPECIFIED beside
        // "Offline-capable" for a store that is in fact hosted.
        let refused = trees
            .bump_store_lease(
                tenant,
                store_id,
                1_777_000_000_000,
                Some("EDGE_PLACEMENT_ON_THE_MOON"),
                store_postgres::StoredRegionWrite::Keep,
                None,
                None,
            )
            .await;
        assert!(
            refused.is_err(),
            "the CHECK constraint must reject a token outside the three modes"
        );
    });
}
