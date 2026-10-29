// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `fleet_row`: a stored fleet row as the console reads it — every column carried, and each that
//! cannot be read degraded to "did not say" rather than failing the fleet read.

use pos_proto::enums::EdgePlacement;
use pos_proto::ids::StoreId;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::WireEnum;
use store_postgres::FleetStoreRow;

use super::fleet_row;
use crate::lease::{Region, StorePlacement};
use crate::registry::EntityStatus;

const STORE: u128 = 0x5701;

fn at(ms: i64) -> Option<Timestamp> {
    Timestamp::from_milliseconds_since_epoch(ms).ok()
}

/// A row with every column set and readable, each instant a different millisecond.
fn full_row() -> FleetStoreRow {
    FleetStoreRow {
        store_id: Ulid::from_u128(STORE).to_string(),
        name: "Ginza".to_owned(),
        status: "archived".to_owned(),
        last_seen_at_ms: Some(1_000),
        last_config_pull_at_ms: Some(2_000),
        config_version_held: Some("held".to_owned()),
        config_version_published: Some("published".to_owned()),
        relay_backlog: 7,
        oldest_pending_at_ms: Some(3_000),
        installed_version: Some("1.4.0".to_owned()),
        self_test_ok: Some(true),
        reported_at_ms: Some(4_000),
        outbox_depth: Some(12),
        outbox_reported_at_ms: Some(5_000),
        lease_generation_held: Some(3),
        lease_reported_at_ms: Some(6_000),
        lease_generation_authoritative: Some(4),
        edge_placement: Some(EdgePlacement::HostedByPlatform.as_wire().to_owned()),
        superseded_generation: Some(2),
        retired_at: Some(7_000),
        retired_by: Some("owner@example.test".to_owned()),
        region_country: Some("SG".to_owned()),
        region_label: Some("ap-southeast-1".to_owned()),
        print_agents: Some(
            r#"[{"agent_device_id":"a1","paired_device_id":"p1","oldest_unacknowledged_secs":9}]"#
                .to_owned(),
        ),
        print_agents_reported_at_ms: Some(8_000),
    }
}

#[test]
fn every_readable_column_reaches_the_fleet_row() {
    let row = fleet_row(full_row()).expect("a readable row");
    assert_eq!(row.store_id, StoreId::new(Ulid::from_u128(STORE)));
    assert_eq!(row.name, "Ginza");
    assert_eq!(row.status, EntityStatus::Archived);
    assert_eq!(row.last_seen_at, at(1_000));
    assert_eq!(row.last_config_pull_at, at(2_000));
    assert_eq!(row.config_version_held.as_deref(), Some("held"));
    assert_eq!(row.config_version_published.as_deref(), Some("published"));
    assert_eq!(row.relay_backlog, 7);
    assert_eq!(row.relay_oldest_pending_at, at(3_000));
    assert_eq!(row.installed_version.as_deref(), Some("1.4.0"));
    assert_eq!(row.self_test_ok, Some(true));
    assert_eq!(row.reported_at, at(4_000));
    assert_eq!(row.outbox_depth, Some(12));
    assert_eq!(row.outbox_reported_at, at(5_000));
    assert_eq!(row.lease_generation_held, Some(3));
    assert_eq!(row.lease_reported_at, at(6_000));
    assert_eq!(row.lease_generation_authoritative, Some(4));
    assert_eq!(
        row.edge_placement,
        StorePlacement::Known(EdgePlacement::HostedByPlatform)
    );
    assert_eq!(row.superseded_generation, Some(2));
    assert_eq!(row.retired_at, at(7_000));
    assert_eq!(row.retired_by.as_deref(), Some("owner@example.test"));
    assert_eq!(row.region, Region::stored("SG", "ap-southeast-1"));
    let agents = row.print_agents.expect("the agents decode");
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].agent_device_id, "a1");
    assert_eq!(agents[0].oldest_unacknowledged_secs, Some(9));
    assert_eq!(row.print_agents_reported_at, at(8_000));
}

#[test]
fn a_column_that_cannot_be_read_says_nothing_rather_than_failing_the_read() {
    let row = fleet_row(FleetStoreRow {
        last_seen_at_ms: Some(i64::MAX),
        last_config_pull_at_ms: Some(i64::MIN),
        oldest_pending_at_ms: Some(i64::MAX),
        reported_at_ms: Some(i64::MAX),
        outbox_reported_at_ms: Some(i64::MAX),
        lease_reported_at_ms: Some(i64::MAX),
        retired_at: Some(i64::MAX),
        print_agents_reported_at_ms: Some(i64::MAX),
        relay_backlog: -1,
        outbox_depth: Some(-1),
        lease_generation_held: Some(-1),
        lease_generation_authoritative: Some(-1),
        superseded_generation: Some(-1),
        edge_placement: Some("EDGE_PLACEMENT_ON_THE_MOON".to_owned()),
        region_label: None,
        print_agents: Some("not json".to_owned()),
        ..full_row()
    })
    .expect("one bad column does not fail the row");
    for instant in [
        row.last_seen_at,
        row.last_config_pull_at,
        row.relay_oldest_pending_at,
        row.reported_at,
        row.outbox_reported_at,
        row.lease_reported_at,
        row.retired_at,
        row.print_agents_reported_at,
    ] {
        assert_eq!(instant, None);
    }
    assert_eq!(row.relay_backlog, 0, "a negative backlog reads as none");
    assert_eq!(row.outbox_depth, None);
    assert_eq!(row.lease_generation_held, None);
    assert_eq!(row.lease_generation_authoritative, None);
    assert_eq!(row.superseded_generation, None);
    assert_eq!(row.edge_placement, StorePlacement::Unreadable);
    assert_eq!(row.region, None, "half a region is no region");
    assert!(row.print_agents.is_none());
}

#[test]
fn absent_columns_read_as_absent() {
    let row = fleet_row(FleetStoreRow {
        edge_placement: None,
        region_country: None,
        region_label: None,
        print_agents: None,
        last_seen_at_ms: None,
        ..full_row()
    })
    .expect("a readable row");
    assert_eq!(row.edge_placement, StorePlacement::NeverIssued);
    assert_eq!(row.region, None);
    assert!(row.print_agents.is_none());
    assert_eq!(row.last_seen_at, None);
}

#[test]
fn a_store_id_that_is_not_one_fails_the_row() {
    assert!(
        fleet_row(FleetStoreRow {
            store_id: "not-a-ulid".to_owned(),
            ..full_row()
        })
        .is_err()
    );
}
