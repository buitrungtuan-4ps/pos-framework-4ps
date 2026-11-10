// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The fleet rows the fleet tests start from, out of `cloud.rs` so it stays inside its size baseline.

use pos_cloud::fleet::FleetRow;
use pos_cloud::lease::StorePlacement;
use pos_cloud::registry::EntityStatus;
use pos_proto::Timestamp;
use pos_proto::ids::StoreId;

use crate::seen_ago;

/// One fleet row in a chosen handover state, so the four wire answers can be asserted side by side.
pub(crate) fn store_in_handover(
    store_id: StoreId,
    authoritative: Option<u64>,
    superseded: Option<u64>,
    retired_at: Option<Timestamp>,
) -> FleetRow {
    FleetRow {
        store_id,
        name: "Thảo Điền".to_owned(),
        status: EntityStatus::Active,
        last_seen_at: Some(seen_ago(1_000)),
        last_config_pull_at: Some(seen_ago(1_000)),
        config_version_held: Some("v-current".to_owned()),
        config_version_published: Some("v-current".to_owned()),
        relay_backlog: 0,
        relay_oldest_pending_at: None,
        installed_version: None,
        self_test_ok: None,
        reported_at: None,
        // Reporting the authoritative generation, empty, from one beat — the shape that reads as
        // settled once a handover has actually happened.
        outbox_depth: Some(0),
        outbox_reported_at: Some(seen_ago(1_000)),
        lease_generation_held: authoritative,
        lease_reported_at: Some(seen_ago(1_000)),
        lease_generation_authoritative: authoritative,
        edge_placement: StorePlacement::NeverIssued,
        region: None,
        superseded_generation: superseded,
        retired_at,
        retired_by: retired_at.map(|_| "01ADMINADMINADMINADMINADMI".to_owned()),
        print_agents: None,
        print_agents_reported_at: None,
        package: None,
        config_wake: None,
        devices: None,
    }
}
