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
///
/// Its minimum edge release is this binary's own: a build nobody stamped is release `0.0.0`
/// ([`crate::version`]), as the browser gate's is, and a fixed minimum above that refused the
/// version on apply, so the store ran the default package and the till never asked for the count.
fn guests_required_package() -> serde_json::Value {
    let version = Ulid::from_u128(401).to_string();
    serde_json::json!({
        "mode": "PACKAGE_MODE_LOCKED",
        "locked_version_id": version,
        "content": {
            "package_id": Ulid::from_u128(400).to_string(),
            "package_version_id": version,
            "version_label": "Demo: guest count required",
            "min_edge_version": crate::version::VERSION,
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

/// Whether this demo store's package reacts on the till — `POS_DEMO_PROFILE=reactions`.
///
/// That profile publishes a version with two reactions (ADR-0193 decision 5): a bill opened on an
/// order with a line prints its check, and a table seated prints an X report. The store enforces
/// permissions, and the demo employee holds every one but the X report's, so the second reaction is
/// never run: the browser gate drives one reaction that runs and one the person may not.
fn reactions() -> bool {
    std::env::var("POS_DEMO_PROFILE").is_ok_and(|profile| profile.eq_ignore_ascii_case("reactions"))
}

/// The X report's permissions, which the `reactions` profile's employee does not hold.
const WITHHELD: [&str; 2] = ["reports.shift.read_x", "reports.takings.view"];

/// A package version, locked, with a reaction the person may run and one they may not, and the
/// default package's optional guest step. Fake ids.
fn reactions_package() -> serde_json::Value {
    let version = Ulid::from_u128(403).to_string();
    serde_json::json!({
        "mode": "PACKAGE_MODE_LOCKED",
        "locked_version_id": version,
        "content": {
            "package_id": Ulid::from_u128(402).to_string(),
            "package_version_id": version,
            "version_label": "Demo: reactions",
            "min_edge_version": crate::version::VERSION,
            // The default package's guest step, optional, so a table is seated as on every store.
            "flows": [{ "body": {
                "flow_id": "dine_in",
                "start_action_id": "sales.table.seat",
                "steps": [{
                    "step_id": "guests",
                    "action_id": "sales.table.seat",
                    "kind": "STEP_KIND_OPTIONAL",
                }],
            }}],
            "reactions": [
                { "body": {
                    "reaction_id": "check_on_bill",
                    "when": "REACTION_TRIGGER_BILL_OPENED",
                    "if": "order.line_count >= 1",
                    "then": { "action_id": "billing.check.print" },
                }},
                { "body": {
                    "reaction_id": "x_on_seat",
                    "when": "REACTION_TRIGGER_ORDER_OPENED",
                    "then": { "action_id": "reports.shift.print_x" },
                }},
            ],
        },
    })
}

/// Whether this demo store's package declares custom data — `POS_DEMO_PROFILE=custom-data`.
///
/// That profile publishes a version with a reservation object type (ADR-0194): a required party
/// size, an occasion chosen from a list, a date and a window-seat flag, none of them personal, and
/// holds the labels a tenant's grid would give them (ADR-0043), so the browser gate can enter,
/// change and read back a record on the till. Every other profile declares no custom data.
pub(crate) fn custom_data() -> bool {
    std::env::var("POS_DEMO_PROFILE")
        .is_ok_and(|profile| profile.eq_ignore_ascii_case("custom-data"))
}

/// The reservation type's version, locked. Fake ids; published as JSON so it goes through the
/// checks a cloud's node does.
fn custom_data_package() -> serde_json::Value {
    let version = Ulid::from_u128(405).to_string();
    serde_json::json!({
        "mode": "PACKAGE_MODE_LOCKED",
        "locked_version_id": version,
        "content": {
            "package_id": Ulid::from_u128(404).to_string(),
            "package_version_id": version,
            "version_label": "Demo: reservations",
            "min_edge_version": crate::version::VERSION,
            "data": {
                "object_types": [{
                    "object_type_id": Ulid::from_u128(410).to_string(),
                    "label_key": "data.reservation",
                    "fields": [
                        { "custom_field_id": Ulid::from_u128(411).to_string(),
                          "label_key": "data.reservation.party",
                          "field_type": "CUSTOM_FIELD_TYPE_INT", "required": true, "personal": false },
                        { "custom_field_id": Ulid::from_u128(412).to_string(),
                          "label_key": "data.reservation.occasion",
                          "field_type": "CUSTOM_FIELD_TYPE_CHOICE", "personal": false,
                          "custom_list_id": Ulid::from_u128(420).to_string() },
                        { "custom_field_id": Ulid::from_u128(413).to_string(),
                          "label_key": "data.reservation.day",
                          "field_type": "CUSTOM_FIELD_TYPE_DATE", "personal": false },
                        { "custom_field_id": Ulid::from_u128(414).to_string(),
                          "label_key": "data.reservation.window",
                          "field_type": "CUSTOM_FIELD_TYPE_BOOL", "personal": false },
                    ],
                }],
                "lists": [{
                    "custom_list_id": Ulid::from_u128(420).to_string(),
                    "label_key": "data.list.occasion",
                    "entries": [
                        { "list_entry_id": Ulid::from_u128(421).to_string(),
                          "label_key": "data.occasion.birthday" },
                        { "list_entry_id": Ulid::from_u128(422).to_string(),
                          "label_key": "data.occasion.business" },
                    ],
                }],
            },
        },
    })
}

/// The `custom-data` profile's receipt template (ADR-0207): a reservation's party size and occasion
/// print on its slip, which is the person's own copy.
fn custom_data_receipt() -> serde_json::Value {
    serde_json::json!({ "templates": { "base": {
        "custom_field_ids": [Ulid::from_u128(411).to_string(), Ulid::from_u128(412).to_string()],
        "person_copy": ["PERSON_COPY_RECORD_SLIP"],
    }}})
}

/// The labels the `custom-data` profile's grid holds, in English and Vietnamese, as a tenant's
/// grid would give them (ADR-0043); `None` on every other profile.
#[must_use]
pub fn custom_data_labels() -> Option<pos_edge_app::labels::LabelGrid> {
    if !custom_data() {
        return None;
    }
    let pairs = [
        ("data.reservation", "Reservation", "Đặt bàn"),
        ("data.reservation.party", "Party size", "Số khách"),
        ("data.reservation.occasion", "Occasion", "Dịp"),
        ("data.reservation.day", "Day", "Ngày"),
        ("data.reservation.window", "Window seat", "Chỗ cạnh cửa sổ"),
        ("data.list.occasion", "Occasions", "Các dịp"),
        ("data.occasion.birthday", "Birthday", "Sinh nhật"),
        ("data.occasion.business", "Business", "Công việc"),
    ];
    let grid = pairs
        .iter()
        .map(|(key, en, vi)| {
            (
                (*key).to_owned(),
                [
                    ("en".to_owned(), (*en).to_owned()),
                    ("vi".to_owned(), (*vi).to_owned()),
                ]
                .into_iter()
                .collect(),
            )
        })
        .collect();
    pos_edge_app::labels::LabelGrid::bounded(grid)
}

/// Publishes the package on its own profile; every other profile runs the release's default
/// package. The `reactions` profile also enforces permissions and withholds the X report's.
pub(super) fn publish_on_its_profile(document: &mut serde_json::Value) {
    if let Some(object) = document.as_object_mut().filter(|_| guests_required()) {
        object.insert("package".to_owned(), guests_required_package());
    }
    if let Some(object) = document.as_object_mut().filter(|_| custom_data()) {
        object.insert("package".to_owned(), custom_data_package());
        object.insert("receipt".to_owned(), custom_data_receipt());
    }
    if reactions() {
        withhold_x_report(document);
        if let Some(object) = document.as_object_mut() {
            object.insert("package".to_owned(), reactions_package());
        }
    }
}

/// Turns enforcement on and takes the X report's permissions from every employee.
fn withhold_x_report(document: &mut serde_json::Value) {
    if let Some(permissions) = document
        .get_mut("permissions")
        .and_then(serde_json::Value::as_object_mut)
    {
        permissions.insert("enforced".to_owned(), serde_json::Value::Bool(true));
        let staff = permissions
            .get_mut("staff")
            .and_then(serde_json::Value::as_array_mut);
        for employee in staff.into_iter().flatten() {
            if let Some(held) = employee
                .get_mut("permissions")
                .and_then(serde_json::Value::as_array_mut)
            {
                held.retain(|id| !id.as_str().is_some_and(|id| WITHHELD.contains(&id)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// The profile's version runs on this binary, stamped or not: applied, it is the store's
    /// package, with its flow.
    #[test]
    fn the_profiles_version_runs_on_any_build() {
        let mut document = serde_json::json!({});
        if let Some(object) = document.as_object_mut() {
            object.insert("package".to_owned(), super::guests_required_package());
        }
        for release in ["0.0.0", "0.14.1"] {
            let edge = pos_core::ota::ReleaseVersion::parse(release);
            let mut stamped = document.clone();
            stamped["package"]["content"]["min_edge_version"] = serde_json::json!(release);
            let content = crate::config_client::checked_package(&stamped, edge)
                .expect("runs here")
                .expect("a package");
            assert_eq!(content.flows.len(), 1, "{release}");
        }
        assert_eq!(
            document["package"]["content"]["min_edge_version"],
            crate::version::VERSION
        );
    }

    /// The custom-data profile's version runs here, with its reservation type and list.
    #[test]
    fn the_custom_data_profile_runs_here() {
        let document = serde_json::json!({ "package": super::custom_data_package() });
        let content = crate::config_client::checked_package(&document, crate::version::released())
            .expect("runs here")
            .expect("a package");
        assert_eq!(content.data.object_types.len(), 1);
        assert_eq!(content.data.lists.len(), 1);
        let receipt: pos_proto::receipt::PublishedReceipt =
            serde_json::from_value(super::custom_data_receipt()).expect("the receipt node parses");
        assert!(
            receipt
                .template_for(pos_proto::SalesChannel::Unspecified)
                .is_some_and(|base| base.is_person_copy(pos_proto::receipt::PersonCopy::RecordSlip))
        );
    }

    /// The reactions profile's version runs here, and its employee no longer holds the X report.
    #[test]
    fn the_reactions_profile_runs_and_withholds_the_x_report() {
        let mut document = serde_json::json!({
            "package": super::reactions_package(),
            "permissions": { "staff": [{ "permissions": ["billing.bill.open", "reports.shift.read_x"] }] },
        });
        let content = crate::config_client::checked_package(&document, crate::version::released())
            .expect("runs here")
            .expect("a package");
        assert_eq!(content.reactions.len(), 2);
        super::withhold_x_report(&mut document);
        assert_eq!(
            document["permissions"],
            serde_json::json!({ "enforced": true, "staff": [{ "permissions": ["billing.bill.open"] }] })
        );
    }
}
