// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An area inside another ([ADR-0211](../../../../../docs/adr/0211-an-area-may-sit-inside-another-area.md)):
//! the `parent_area_id` column round-trips, a refused check writes nothing, and two moves on one
//! store run one after the other, so two writes that are each fine alone cannot together make a
//! cycle.

use super::{block_on, prepared};
use store_postgres::{AreaFields, AreaLinkRow, AreaWrite, PostgresFloor};

const TENANT: &str = "0000000000TENANTFLOORTREEA";
const STORE: &str = "000000000000000STOREFLOORT";
const UPPER: &str = "000000000000000AREAUPPERAA";
const LOWER: &str = "000000000000000AREALOWERAA";

/// Whether making `id` sit inside `parent` would close a loop among `links`: the test's own small
/// rule, standing in for the cloud's.
fn closes_a_loop(links: &[AreaLinkRow], id: &str, parent: &str) -> Option<&'static str> {
    let mut cursor = Some(parent.to_owned());
    for _ in 0..=links.len() {
        let current = cursor?;
        if current == id {
            return Some("cycle");
        }
        cursor = links
            .iter()
            .find(|link| link.id == current)
            .and_then(|link| link.parent_area_id.clone());
    }
    None
}

fn fields<'a>(id: &'a str, name: &'a str, parent: Option<&'a str>) -> AreaFields<'a> {
    AreaFields {
        tenant_id: TENANT,
        id,
        name,
        status: "active",
        parent_area_id: parent,
    }
}

async fn version_of(floor: &PostgresFloor, id: &str) -> String {
    floor
        .fetch_area(TENANT, id)
        .await
        .expect("read the area")
        .expect("the area is there")
        .version
}

#[test]
fn a_parent_round_trips_and_a_refused_write_changes_nothing() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let floor = store.floor();
        for (id, name, parent) in [(UPPER, "Tầng 2", None), (LOWER, "VIP", Some(UPPER))] {
            let written = floor
                .insert_area(STORE, fields(id, name, parent), |_| None::<()>)
                .await
                .expect("insert");
            assert!(matches!(written, AreaWrite::Written(_)), "{name}");
        }
        let lower = floor
            .fetch_area(TENANT, LOWER)
            .await
            .expect("read")
            .expect("there");
        assert_eq!(lower.parent_area_id.as_deref(), Some(UPPER));

        // The check sees the store's areas with their parents, and its refusal writes nothing.
        let before = version_of(&floor, UPPER).await;
        let refused = floor
            .set_area(fields(UPPER, "Tầng 2", Some(LOWER)), &before, |links| {
                assert_eq!(links.len(), 2, "the check reads the whole store");
                closes_a_loop(links, UPPER, LOWER)
            })
            .await
            .expect("the write must not raise");
        assert_eq!(refused, AreaWrite::Refused("cycle"));
        assert_eq!(
            version_of(&floor, UPPER).await,
            before,
            "nothing was written"
        );

        // A stale version is a mismatch before the check runs; an unknown area is not found.
        let stale = floor
            .set_area(fields(UPPER, "Tầng 2", None), "1", |_| Some("checked"))
            .await
            .expect("stale");
        assert_eq!(stale, AreaWrite::VersionMismatch);
        let missing = floor
            .set_area(fields("000000000000000AREANOPEAAA", "x", None), "1", |_| {
                None::<()>
            })
            .await
            .expect("missing");
        assert_eq!(missing, AreaWrite::NotFound);

        // Moving the lower area to the top clears its parent.
        let version = version_of(&floor, LOWER).await;
        let moved = floor
            .set_area(fields(LOWER, "VIP", None), &version, |_| None::<()>)
            .await
            .expect("move");
        assert!(matches!(moved, AreaWrite::Written(_)));
        let lower = floor
            .fetch_area(TENANT, LOWER)
            .await
            .expect("read")
            .expect("there");
        assert_eq!(lower.parent_area_id, None);
    });
}

#[test]
fn two_moves_that_would_close_a_loop_together_run_one_after_the_other() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let floor = store.floor();
        for id in [UPPER, LOWER] {
            floor
                .insert_area(STORE, fields(id, "Area", None), |_| None::<()>)
                .await
                .expect("insert");
        }
        let upper_version = version_of(&floor, UPPER).await;
        let lower_version = version_of(&floor, LOWER).await;
        // Each move is fine on the store as it stands; the two together would be a loop.
        let (first, second) = tokio::join!(
            floor.set_area(
                fields(UPPER, "Area", Some(LOWER)),
                &upper_version,
                |links| { closes_a_loop(links, UPPER, LOWER) }
            ),
            floor.set_area(
                fields(LOWER, "Area", Some(UPPER)),
                &lower_version,
                |links| { closes_a_loop(links, LOWER, UPPER) }
            ),
        );
        let outcomes = [first.expect("first"), second.expect("second")];
        let written = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, AreaWrite::Written(_)))
            .count();
        assert_eq!(written, 1, "{outcomes:?}");
        assert!(
            outcomes.contains(&AreaWrite::Refused("cycle")),
            "{outcomes:?}"
        );
    });
}
