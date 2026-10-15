// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Releases: create, list, read one, the status roll-up, and the in-flight scan (ADR-0125).

use store_postgres::NewReleaseRow;

use super::{TENANT_A, TENANT_B, block_on, prepared};

const WALL: &str = "01RELWALLAAAAAAAAAAAAAAAAA";
const INSTANT: &str = "01RELINSTANTAAAAAAAAAAAAAA";

fn wall_clock<'a>(id: &'a str, tenant_id: &'a str, name: &'a str) -> NewReleaseRow<'a> {
    NewReleaseRow {
        id,
        tenant_id,
        name,
        status: "DRAFT",
        target_group_id: Some("01GROUPAAAAAAAAAAAAAAAAAAA"),
        wall_clock_date: Some("2027-02-08"),
        wall_clock_time: Some("04:00"),
        instant_at_ms: None,
        created_by: "admin-1",
    }
}

fn instant<'a>(id: &'a str, tenant_id: &'a str, name: &'a str) -> NewReleaseRow<'a> {
    NewReleaseRow {
        id,
        tenant_id,
        name,
        status: "DRAFT",
        target_group_id: None,
        wall_clock_date: None,
        wall_clock_time: None,
        instant_at_ms: Some(1_800_000_000_000),
        created_by: "admin-1",
    }
}

/// The two kinds of moment survive a round trip and stay distinguishable, the roll-up moves, and
/// neither the tenant-scoped reads nor the fleet-wide in-flight scan mixes tenants up.
///
/// The two kinds matter because only one of them is safe at a store whose timezone nobody
/// recorded ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md) §2): a
/// wall-clock release is refused there by name, an instant release is not. If the columns came
/// back indistinguishable the refusal could not be made.
#[test]
fn the_two_kinds_of_moment_survive_and_the_rollup_moves() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let releases = store.config_releases();

        releases
            .create(&wall_clock(WALL, TENANT_A, "Tết 2027"))
            .await
            .expect("wall clock");
        releases
            .create(&instant(INSTANT, TENANT_A, "Price fix"))
            .await
            .expect("instant");
        // Another tenant's release, which must not appear in tenant A's list.
        releases
            .create(&instant(
                "01RELOTHERAAAAAAAAAAAAAAAA",
                TENANT_B,
                "Someone else's",
            ))
            .await
            .expect("other tenant");

        let listed = releases.list_for_tenant(TENANT_A).await.expect("list");
        assert_eq!(listed.len(), 2, "the other tenant's release is not listed");

        let wall = releases
            .fetch_one(TENANT_A, WALL)
            .await
            .expect("read")
            .expect("the wall-clock release");
        assert_eq!(
            wall.name, "Tết 2027",
            "the operator's name comes back as typed"
        );
        assert_eq!(wall.wall_clock_date.as_deref(), Some("2027-02-08"));
        assert_eq!(wall.wall_clock_time.as_deref(), Some("04:00"));
        assert_eq!(
            wall.instant_at_ms, None,
            "a wall-clock release has no single instant: its instants are per store"
        );
        assert_eq!(
            wall.target_group_id.as_deref(),
            Some("01GROUPAAAAAAAAAAAAAAAAAAA"),
            "the cohort it was aimed at is recorded for the report"
        );

        let moment = releases
            .fetch_one(TENANT_A, INSTANT)
            .await
            .expect("read")
            .expect("the instant release");
        assert_eq!(moment.instant_at_ms, Some(1_800_000_000_000));
        assert_eq!(
            moment.wall_clock_date, None,
            "an instant release carries no local date to resolve"
        );

        // Nothing is in flight while both are drafts — a draft fires nothing.
        assert!(
            releases.in_flight().await.expect("in flight").is_empty(),
            "a draft is not in flight"
        );

        assert!(
            releases
                .set_status(TENANT_A, WALL, "SCHEDULED")
                .await
                .expect("schedule it")
        );
        let in_flight = releases.in_flight().await.expect("in flight");
        assert_eq!(in_flight.len(), 1);
        assert_eq!(in_flight.first().expect("row").id, WALL);

        // A terminal status leaves the in-flight scan again.
        assert!(
            releases
                .set_status(TENANT_A, WALL, "PARTIAL")
                .await
                .expect("settle it")
        );
        assert!(
            releases.in_flight().await.expect("in flight").is_empty(),
            "a settled release is no longer in flight"
        );

        // A release of another tenant cannot be moved through this tenant's handle.
        assert!(
            !releases
                .set_status(TENANT_A, "01RELOTHERAAAAAAAAAAAAAAAA", "SCHEDULED")
                .await
                .expect("cross-tenant write"),
            "the write names the tenant, so it changes nothing"
        );
        assert!(
            releases
                .fetch_one(TENANT_A, "01RELOTHERAAAAAAAAAAAAAAAA")
                .await
                .expect("cross-tenant read")
                .is_none(),
            "and the read cannot see it either"
        );
    });
}
