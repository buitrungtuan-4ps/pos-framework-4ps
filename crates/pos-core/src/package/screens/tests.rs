// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_proto::package::{TimeSlot, Variant};
use pos_proto::screen::{DEPTH_MAX, NODES_MAX, SCREENS_MAX, Screen};

use super::{ScreenFault, ScreenRefusal, check_screens};
use crate::package::VariantRefusal;

/// A base variant of a screen read from its JSON.
fn screen(json: &str) -> Variant<Screen> {
    serde_json::from_str(&format!(r#"{{"body":{json}}}"#)).expect("a screen")
}

/// A variant for one device class.
fn for_class(class: &str, json: &str) -> Variant<Screen> {
    serde_json::from_str(&format!(
        r#"{{"selector":{{"device_class":"{class}"}},"body":{json}}}"#
    ))
    .expect("a screen")
}

fn check(screens: &[Variant<Screen>]) -> Result<(), ScreenRefusal> {
    check_screens(screens, &[] as &[TimeSlot])
}

fn fault(screens: &[Variant<Screen>]) -> Option<ScreenFault> {
    check(screens).err().map(|refusal| refusal.fault)
}

const ORDER: &str = r#"{"screen_id":"table_order","root":{"component_id":"stack","children":[
    {"component_id":"text","properties":{"text_key":"order.title","variant":"title"}},
    {"component_id":"action_button","properties":{"label_key":"order.send","variant":"primary",
        "action":{"action_id":"sales.order.fire"}}},
    {"component_id":"action_button","properties":{"label_key":"order.void",
        "action":{"action_id":"sales.line.void","params":{"order_line_id":"01J0000000000000000000000L","reason_code_id":"01J0000000000000000000000R"}}}}
]}}"#;

/// One node: `{"component_id":…}` with what follows inside it.
fn one(node: &str) -> Variant<Screen> {
    screen(&format!(r#"{{"screen_id":"s","root":{node}}}"#))
}

#[test]
fn a_screen_of_catalogued_components_with_literal_properties_passes() {
    assert_eq!(check(&[screen(ORDER)]), Ok(()));
    assert_eq!(check(&[]), Ok(()));
}

#[test]
fn a_screen_id_is_a_short_snake_case_token() {
    let bad = screen(r#"{"screen_id":"Table Order","root":{"component_id":"stack"}}"#);
    assert_eq!(fault(&[bad]), Some(ScreenFault::ScreenId));
}

#[test]
fn a_package_has_at_most_its_screens_and_a_screen_its_nodes_and_levels() {
    let many: Vec<_> = (0..=SCREENS_MAX)
        .map(|n| {
            screen(&format!(
                r#"{{"screen_id":"s{n}","root":{{"component_id":"stack"}}}}"#
            ))
        })
        .collect();
    assert_eq!(fault(&many), Some(ScreenFault::TooManyScreens));

    let leaf = r#"{"component_id":"text","properties":{"text_key":"k"}}"#;
    let wide = |count: usize| {
        let children = vec![leaf; count].join(",");
        one(&format!(
            r#"{{"component_id":"stack","children":[{children}]}}"#
        ))
    };
    assert_eq!(check(&[wide(NODES_MAX - 1)]), Ok(()));
    assert_eq!(fault(&[wide(NODES_MAX)]), Some(ScreenFault::TooManyNodes));

    let deep = |levels: usize| {
        let mut node = leaf.to_owned();
        for _ in 1..levels {
            node = format!(r#"{{"component_id":"stack","children":[{node}]}}"#);
        }
        one(&node)
    };
    assert_eq!(check(&[deep(DEPTH_MAX)]), Ok(()));
    assert_eq!(fault(&[deep(DEPTH_MAX + 1)]), Some(ScreenFault::TooDeep));
}

#[test]
fn an_unknown_component_or_children_on_a_leaf_is_refused() {
    assert_eq!(
        fault(&[one(
            r#"{"component_id":"stack","children":[{"component_id":"teleporter"}]}"#
        )]),
        Some(ScreenFault::UnknownComponent(1))
    );
    assert_eq!(
        fault(&[one(
            r#"{"component_id":"text","properties":{"text_key":"k"},"children":[{"component_id":"stack"}]}"#
        )]),
        Some(ScreenFault::Children(0))
    );
}

#[test]
fn a_property_is_one_the_component_takes_of_its_own_kind() {
    let text = |properties: &str| {
        one(&format!(
            r#"{{"component_id":"text","properties":{properties}}}"#
        ))
    };
    assert_eq!(
        fault(&[text(r#"{"text_key":"k","colour":"red"}"#)]),
        Some(ScreenFault::UnknownProperty(0))
    );
    assert_eq!(
        fault(&[text("{}")]),
        Some(ScreenFault::MissingProperty(0, "text_key"))
    );
    for bad in [
        r#"{"text_key":"Not A Key"}"#,
        r#"{"text_key":7}"#,
        r#"{"text_key":"k","variant":"shouting"}"#,
        r#"{"text_key":"k","variant":true}"#,
    ] {
        assert!(
            matches!(fault(&[text(bad)]), Some(ScreenFault::PropertyValue(0, _))),
            "{bad}"
        );
    }
}

#[test]
fn an_action_property_names_a_catalogued_act_and_fixes_only_what_it_may() {
    let button = |action: &str| {
        one(&format!(
            r#"{{"component_id":"action_button","properties":{{"label_key":"k","action":{action}}}}}"#
        ))
    };
    assert_eq!(
        fault(&[button(r#"{"action_id":"sales.line.teleport"}"#)]),
        Some(ScreenFault::UnknownAction(0, "action"))
    );
    // A parameter the act does not take, one of the wrong kind, an amount, an entry.
    for bad in [
        r#"{"action_id":"sales.order.fire","params":{"colour":"red"}}"#,
        r#"{"action_id":"sales.order.fire","params":{"station_id":3}}"#,
        r#"{"action_id":"cash.movement.paid_in","params":{"shift_id":"x","amount":500,"reason_code_id":"r"}}"#,
        r#"{"action_id":"billing.bill.settle","params":{"bill_id":"b","payments":"cash"}}"#,
    ] {
        assert_eq!(
            fault(&[button(bad)]),
            Some(ScreenFault::FixedParam(0, "action")),
            "{bad}"
        );
    }
    // The screen supplies the order and the table; nothing supplies a line or a reason.
    assert_eq!(
        check(&[button(r#"{"action_id":"sales.order.fire"}"#)]),
        Ok(())
    );
    assert_eq!(
        fault(&[button(r#"{"action_id":"sales.line.void"}"#)]),
        Some(ScreenFault::MissingParam(0, "order_line_id"))
    );
}

#[test]
fn a_screens_variants_are_told_apart_and_drawn_on_their_class() {
    let tablet = for_class("DEVICE_CLASS_TABLET", ORDER);
    assert_eq!(check(&[screen(ORDER), tablet.clone()]), Ok(()));
    assert_eq!(
        check(&[screen(ORDER), tablet.clone(), tablet.clone()]),
        Err(ScreenRefusal {
            screen: 0,
            fault: ScreenFault::Variants(VariantRefusal::SameSelector {
                first: 1,
                second: 2
            }),
        })
    );
    assert_eq!(
        fault(&[tablet]),
        Some(ScreenFault::Variants(VariantRefusal::NoBaseVariant))
    );
}
