// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An answer's schema says which fields are always there, on the till's document as on the cloud's
//! (ADR-0177 decision 3).
//!
//! A field the edge always sends is `required`, `null` or not; a field it leaves out when empty is
//! not `required`, and never `null`. So no property of a schema an answer reaches may be both
//! optional and nullable, which would leave the till unable to tell "absent" from "null". Request
//! bodies are not held to it: a request may leave an optional field out or send `null`.
//!
//! [`MAY_BE_ABSENT`] lists the properties that are both today. It only shrinks, and goes when it is
//! empty. Every entry is a backbone type the till's answers carry, a `pos-proto` wire type or the
//! printer port's job, whose fields wait for an owner-reviewed change to that crate.

use std::collections::BTreeSet;

use serde_json::Value;
use utoipa::OpenApi as _;

use super::openapi::EdgeApiDoc;

/// `Schema.property` in the till's document that an answer reaches and that is both optional and
/// nullable.
const MAY_BE_ABSENT: &[&str] = &[
    "FloorTable.position",
    "KitchenStation.backup_station_id",
    "KitchenStation.late_after_seconds",
    "PrintJob.station_id",
    "RoutingRule.course_id",
    "RoutingRule.menu_item_id",
    "StationPlan.default_station_id",
];

/// Every `#/components/schemas/…` name `value` refers to.
fn references(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if let Some(Value::String(target)) = object.get("$ref")
                && let Some(name) = target.strip_prefix("#/components/schemas/")
            {
                found.push(name.to_owned());
            }
            object.values().for_each(|child| references(child, found));
        }
        Value::Array(items) => items.iter().for_each(|child| references(child, found)),
        _ => {}
    }
}

/// Whether a property's schema admits `null`.
fn nullable(property: &Value) -> bool {
    let typed = property["type"]
        .as_array()
        .is_some_and(|types| types.iter().any(|t| t == "null"));
    let alternative = ["oneOf", "anyOf"].iter().any(|key| {
        property[*key]
            .as_array()
            .is_some_and(|parts| parts.iter().any(|part| part["type"] == "null"))
    });
    typed || alternative
}

/// The optional and nullable properties of one schema, its `allOf` and `oneOf` parts included.
fn optional_and_nullable(name: &str, schema: &Value, found: &mut BTreeSet<String>) {
    let required: BTreeSet<&str> = schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    for (property, value) in schema["properties"].as_object().into_iter().flatten() {
        if !required.contains(property.as_str()) && nullable(value) {
            found.insert(format!("{name}.{property}"));
        }
    }
    for part in ["allOf", "oneOf"] {
        for piece in schema[part].as_array().into_iter().flatten() {
            optional_and_nullable(name, piece, found);
        }
    }
}

/// The optional and nullable properties of every schema a success answer reaches.
fn ambiguous() -> BTreeSet<String> {
    let json = serde_json::to_value(EdgeApiDoc::openapi()).expect("serialize the document");
    let schemas = &json["components"]["schemas"];
    let mut pending = Vec::new();
    for item in json["paths"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, item)| item)
    {
        for operation in item.as_object().into_iter().flatten().map(|(_, op)| op) {
            for (status, response) in operation["responses"].as_object().into_iter().flatten() {
                if status.starts_with('2') {
                    references(response, &mut pending);
                }
            }
        }
    }
    let mut reached = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if reached.insert(name.clone()) {
            references(&schemas[&name], &mut pending);
        }
    }
    let mut found = BTreeSet::new();
    for name in &reached {
        optional_and_nullable(name, &schemas[name], &mut found);
    }
    found
}

#[test]
fn no_till_answer_field_is_both_optional_and_nullable() {
    let listed: BTreeSet<String> = MAY_BE_ABSENT
        .iter()
        .map(|&entry| entry.to_owned())
        .collect();
    assert_eq!(
        ambiguous(),
        listed,
        "a field an answer always sends is `#[schema(required = true)]`; one it skips when empty is \
         `#[schema(nullable = false)]`. A fixed entry leaves MAY_BE_ABSENT"
    );
}
