// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The release development, replay and simulator edges are stamped with (the justfile's
//! `dev_edge_release`) is at least what every package version this tree publishes needs
//! (ADR-0189 decision 5). Otherwise those edges would refuse every package, and nothing short of a
//! release could test one.

use pos_cloud_config::packages::PACKAGE_NODE_SINCE;
use pos_core::ota::ReleaseVersion;

const JUSTFILE: &str = include_str!("../../../justfile");

#[test]
fn the_dev_edge_release_runs_what_this_tree_publishes() {
    let stamp = JUSTFILE
        .lines()
        .find_map(|line| line.strip_prefix("dev_edge_release := "))
        .map(|value| value.trim().trim_matches('"'))
        .expect("the justfile names dev_edge_release");
    let stamp = ReleaseVersion::parse(stamp).expect("dev_edge_release is X.Y.Z, with no `v`");
    let floor = ReleaseVersion::parse(PACKAGE_NODE_SINCE).expect("the floor parses");
    assert!(
        stamp >= floor,
        "dev_edge_release is below {PACKAGE_NODE_SINCE}, so dev edges would refuse every package"
    );
}
