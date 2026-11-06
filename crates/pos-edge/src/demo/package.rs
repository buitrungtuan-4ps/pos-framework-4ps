// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The demo store's package profile: a version whose flow makes the guest count required
//! (ADR-0192), so the browser gate can drive a till that will not take a first line without one.

use pos_proto::ulid::Ulid;

/// Whether this demo store's package asks a guest count before a table's first line, as a required
/// step the edge re-checks — `POS_DEMO_PROFILE=guests-required`.
///
/// That profile publishes a `package` node, locked to one version whose `dine_in` flow makes the
/// guest count a required step (ADR-0192), so the browser gate can drive a till that will not take a
/// first line without a count. Every other profile runs the release's default package, whose guest
/// count is optional.
fn guests_required() -> bool {
    std::env::var("POS_DEMO_PROFILE")
        .is_ok_and(|profile| profile.eq_ignore_ascii_case("guests-required"))
}

/// A package version, locked, whose `dine_in` flow asks the guest count as a required step before a
/// table's first line, which the edge re-checks from the order's own count (ADR-0192 decision 4).
/// Fake ids; published as JSON, as the floor is, so it goes through the checks a cloud's node does.
fn guests_required_package() -> serde_json::Value {
    let version = Ulid::from_u128(401).to_string();
    serde_json::json!({
        "mode": "PACKAGE_MODE_LOCKED",
        "locked_version_id": version,
        "content": {
            "package_id": Ulid::from_u128(400).to_string(),
            "package_version_id": version,
            "version_label": "Demo: guest count required",
            "min_edge_version": "0.0.1",
            "flows": [{ "body": {
                "flow_id": "dine_in",
                "start_action_id": "sales.table.seat",
                "steps": [
                    {
                        "step_id": "guests",
                        "action_id": "sales.table.seat",
                        "kind": "STEP_KIND_REQUIRED",
                        "done_when": "has(order.guest_count) && order.guest_count > 0",
                    },
                    {
                        "step_id": "first_line",
                        "action_id": "sales.line.add",
                        "kind": "STEP_KIND_REQUIRED",
                        "prerequisites": ["guests"],
                    },
                ],
            }}],
        },
    })
}

/// Publishes the package on its own profile; every other profile runs the release's default
/// package.
pub(super) fn publish_on_its_profile(document: &mut serde_json::Value) {
    if let Some(object) = document.as_object_mut().filter(|_| guests_required()) {
        object.insert("package".to_owned(), guests_required_package());
    }
}
