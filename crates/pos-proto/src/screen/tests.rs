// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use std::collections::BTreeSet;

use super::{
    CATALOGUE, Component, ParamValue, PropertyKind, PropertyValue, SNAPSHOT_PATH, Screen,
    render_snapshot, render_typescript,
};
use crate::actions::{Action, ActionParamKind};
use crate::devices::DeviceClass;

fn is_snake(word: &str) -> bool {
    !word.is_empty()
        && word.starts_with(|c: char| c.is_ascii_lowercase())
        && word
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Every id and property name is `snake_case` and listed once, and `find` answers it.
#[test]
fn every_component_is_named_once_in_snake_case() {
    let mut seen = BTreeSet::new();
    for component in CATALOGUE {
        let id = component.component_id;
        assert!(is_snake(id), "{id} is snake_case");
        assert!(seen.insert(id), "{id} is listed once");
        assert_eq!(
            Component::find(id).map(|found| found.component_id),
            Some(id)
        );
        assert!(!component.summary.is_empty(), "{id} says what it shows");
        assert!(!component.since.is_empty(), "{id} names its release");
        let mut names = BTreeSet::new();
        for property in component.properties {
            assert!(is_snake(property.name), "{id}: {}", property.name);
            assert!(names.insert(property.name), "{id}: {} once", property.name);
            assert_eq!(component.property(property.name), Some(property));
        }
    }
    assert!(Component::find("teleporter").is_none());
}

/// A property is one of the five literal kinds, a token lists its values and only a token does,
/// and only a whole number has bounds (ADR-0203 decision 2).
#[test]
fn every_property_is_a_literal_of_a_known_kind() {
    for component in CATALOGUE {
        for property in component.properties {
            let id = format!("{}.{}", component.component_id, property.name);
            assert_ne!(property.kind, PropertyKind::Unspecified, "{id}");
            assert_eq!(
                property.kind == PropertyKind::Token,
                !property.tokens.is_empty(),
                "{id}: a token, and only a token, lists its values"
            );
            for token in property.tokens {
                assert!(is_snake(token), "{id}: {token}");
            }
            let (min, max) = property.bounds;
            if property.kind == PropertyKind::Int {
                assert!(min <= max, "{id}: bounds in order");
            } else {
                assert_eq!((min, max), (0, 0), "{id}: only a number has bounds");
            }
        }
    }
}

/// A component supplies only parameters some act takes as an id, and an act it may bind can be
/// completed from what it supplies (ADR-0203 decision 3).
#[test]
fn what_a_component_supplies_is_an_id_some_act_takes() {
    for component in CATALOGUE {
        for param in component.supplies {
            let taken = crate::actions::CATALOGUE.iter().any(|action| {
                action.params.iter().any(|p| {
                    p.name == *param
                        && matches!(p.kind, ActionParamKind::Id | ActionParamKind::IdList)
                })
            });
            assert!(taken, "{}: no act takes {param}", component.component_id);
        }
    }
}

/// A component is drawn on at least one class, never on the unknown one, and its fallback is a
/// component of the catalogue drawn on every class it is.
#[test]
fn every_component_names_its_classes_and_a_fallback_it_can_reach() {
    for component in CATALOGUE {
        let id = component.component_id;
        assert!(!component.device_classes.is_empty(), "{id}");
        assert!(
            !component.device_classes.contains(&DeviceClass::Unspecified),
            "{id}"
        );
        if let Some(fallback) = component.fallback_component {
            let found = Component::find(fallback);
            assert!(found.is_some(), "{id}: {fallback} is catalogued");
            assert_ne!(fallback, id, "{id} does not fall back to itself");
        }
    }
}

/// A screen reads from its JSON and writes the same JSON back: literals untagged, empty parts left
/// off, an act's fixed parameters by name.
#[test]
fn a_screen_round_trips_through_its_json() {
    let json = r#"{"screen_id":"table_order","root":{"component_id":"stack","properties":{"direction":"vertical"},"children":[{"component_id":"text","properties":{"text_key":"order.title"}},{"component_id":"action_button","properties":{"action":{"action_id":"sales.order.fire","params":{"station_id":"01J0000000000000000000000S"}},"label_key":"order.send","variant":"primary"}}]}}"#;
    let screen: Screen = serde_json::from_str(json).expect("a screen");
    let button = screen.root.children.get(1).expect("the button");
    let Some(PropertyValue::Action(binding)) = button.properties.get("action") else {
        panic!("an action property");
    };
    assert_eq!(binding.action_id, "sales.order.fire");
    assert!(Action::find(&binding.action_id).is_some());
    assert!(matches!(
        binding.params.get("station_id"),
        Some(ParamValue::Text(_))
    ));
    assert_eq!(
        button.properties.get("variant"),
        Some(&PropertyValue::Text("primary".to_owned()))
    );
    assert_eq!(serde_json::to_string(&screen).expect("json"), json);
}

/// A field from a newer release is ignored, not refused, as on every published node.
#[test]
fn a_screen_from_a_newer_release_still_reads() {
    let json = r#"{"screen_id":"s","root":{"component_id":"text","properties":{"text_key":"k"},"layout_hint":"x"},"home":true}"#;
    let screen: Screen = serde_json::from_str(json).expect("a screen");
    assert_eq!(screen.root.component_id, "text");
}

#[test]
fn the_committed_snapshot_matches_the_catalogue() {
    let rendered = render_snapshot();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(SNAPSHOT_PATH);
    // Set POS_UPDATE_SNAPSHOTS=1 to rewrite; a check that silently fixes itself is not a check.
    if std::env::var("POS_UPDATE_SNAPSHOTS").is_ok() {
        std::fs::write(&path, &rendered).expect("write the snapshot");
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == rendered,
        "the component catalogue no longer matches {SNAPSHOT_PATH}. Regenerate it with:\n\
         \n    POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-proto snapshot\n"
    );
}

/// The module both front ends build from is the component catalogue as rendered, so neither can drift from
/// the validator.
#[test]
fn the_tills_snapshot_of_the_components_matches_it() {
    let rendered = render_typescript();
    let module = super::TYPESCRIPT_PATH;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(module);
    if std::env::var("POS_UPDATE_SNAPSHOTS").is_ok() {
        std::fs::write(&path, &rendered).expect("write the module");
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == rendered,
        "{module} no longer matches the component catalogue. Regenerate it with:\n\
         \n    POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-proto snapshot\n"
    );
}
