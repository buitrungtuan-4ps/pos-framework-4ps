// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use serde_json::json;

use super::{
    ListRefusal, PublishedOrderChannels, built_in_types, check_order_types, check_sources,
    tokens_of,
};
use crate::sale_channel::{OrderType, SaleSource, SaleToken};

fn node(value: serde_json::Value) -> PublishedOrderChannels {
    serde_json::from_value(value).expect("the node reads")
}

fn field(value: &serde_json::Value, name: &str) -> serde_json::Value {
    value.get(name).cloned().unwrap_or_default()
}

fn token(text: &str) -> SaleToken {
    SaleToken::order_type(text)
        .or_else(|_| SaleToken::source(text))
        .expect("a token")
}

/// Fake configuration only: a pick-up type and a hotline, named in English and Vietnamese.
fn pickup_store() -> serde_json::Value {
    json!({
        "order_types": [
            { "order_type": "ORDER_TYPE_DINE_IN", "base_order_type": "ORDER_TYPE_DINE_IN" },
            { "order_type": "ORDER_TYPE_TAKEAWAY", "base_order_type": "ORDER_TYPE_TAKEAWAY", "default": true },
            { "order_type": "ORDER_TYPE_PICKUP", "base_order_type": "ORDER_TYPE_TAKEAWAY",
              "names": { "en": "Pick-up", "vi": "Tự đến lấy" }, "applies_to_sources": ["SALE_SOURCE_APP"] },
            { "order_type": "ORDER_TYPE_CATERING", "base_order_type": "ORDER_TYPE_DELIVERY",
              "names": { "en": "Catering" }, "active": false }
        ],
        "sources": [
            { "source": "SALE_SOURCE_HOTLINE", "names": { "en": "Hotline", "vi": "Tổng đài" } },
            { "source": "SALE_SOURCE_KIOSK", "names": { "en": "Kiosk", "vi": "Ki-ốt" } },
            { "source": "SALE_SOURCE_WEBSITE", "names": { "en": "Website" }, "active": false }
        ]
    })
}

#[test]
fn a_store_that_lists_nothing_offers_the_three_built_ins() {
    for empty in [
        json!({}),
        json!({ "order_types": [] }),
        json!({ "order_types": 7 }),
    ] {
        let read = node(empty);
        assert_eq!(read.order_types(), built_in_types());
        assert_eq!(
            read.active_base(&SaleToken::of(OrderType::Takeaway)),
            Some(OrderType::Takeaway)
        );
        assert!(read.till_may_record(&SaleToken::POS));
        assert!(!read.till_may_record(&token("SALE_SOURCE_HOTLINE")));
    }
    assert_eq!(
        serde_json::to_value(PublishedOrderChannels::default()).expect("serialise"),
        json!({})
    );
}

#[test]
fn a_published_type_maps_onto_its_base_kind_while_it_is_active() {
    let read = node(pickup_store());
    assert_eq!(
        read.active_base(&token("ORDER_TYPE_PICKUP")),
        Some(OrderType::Takeaway)
    );
    assert_eq!(read.active_base(&token("ORDER_TYPE_CATERING")), None);
    assert_eq!(read.active_base(&token("ORDER_TYPE_DRIVE_THROUGH")), None);
}

#[test]
fn the_till_records_its_own_or_an_active_custom_source_only() {
    let read = node(pickup_store());
    assert!(read.till_may_record(&SaleToken::POS));
    assert!(read.till_may_record(&token("SALE_SOURCE_HOTLINE")));
    assert!(
        !read.till_may_record(&token("SALE_SOURCE_WEBSITE")),
        "inactive"
    );
    assert!(
        !read.till_may_record(&SaleToken::KIOSK),
        "a kiosk records its own"
    );
    assert!(!read.till_may_record(&SaleToken::of(SaleSource::Qr)));
}

#[test]
fn an_apps_order_takes_the_type_that_applies_to_apps_with_its_base() {
    let read = node(pickup_store());
    assert_eq!(
        read.matched_type(SaleSource::App, OrderType::Takeaway),
        Some(token("ORDER_TYPE_PICKUP"))
    );
    assert_eq!(
        read.matched_type(SaleSource::App, OrderType::Delivery),
        None
    );
    assert_eq!(read.matched_type(SaleSource::Qr, OrderType::Takeaway), None);
}

#[test]
fn names_are_by_token_for_what_the_store_named() {
    let names = node(pickup_store()).names();
    assert_eq!(
        names
            .get("ORDER_TYPE_PICKUP")
            .and_then(|by_language| by_language.get("vi"))
            .map(crate::text::DisplayName::as_str),
        Some("Tự đến lấy")
    );
    assert!(names.contains_key("SALE_SOURCE_KIOSK"));
    assert!(
        !names.contains_key("ORDER_TYPE_DINE_IN"),
        "a built-in left unnamed"
    );
}

#[test]
fn the_cloud_takes_a_sound_list_and_the_default() {
    let store = pickup_store();
    assert_eq!(check_order_types(&field(&store, "order_types")), Ok(()));
    assert_eq!(check_sources(&field(&store, "sources")), Ok(()));
    let default = serde_json::to_value(built_in_types()).expect("serialise");
    assert_eq!(check_order_types(&default), Ok(()));
    assert_eq!(check_sources(&json!([])), Ok(()));
}

#[test]
fn the_cloud_refuses_a_list_it_could_not_run() {
    let refused = [
        json!({}),
        json!([{ "order_type": "ORDER_TYPE_PICKUP", "base_order_type": "ORDER_TYPE_TAKEAWAY" }]),
        json!([{ "order_type": "ORDER_TYPE_PICKUP", "base_order_type": "ORDER_TYPE_PICKUP",
                 "names": { "en": "Pick-up" } }]),
        json!([{ "order_type": "ORDER_TYPE_DINE_IN", "base_order_type": "ORDER_TYPE_TAKEAWAY" }]),
        json!([{ "order_type": "ORDER_TYPE_UNSPECIFIED", "base_order_type": "ORDER_TYPE_DINE_IN" }]),
        json!([{ "order_type": "SALE_SOURCE_POS", "base_order_type": "ORDER_TYPE_DINE_IN" }]),
        json!([{ "order_type": "ORDER_TYPE_DINE_IN", "base_order_type": "ORDER_TYPE_DINE_IN" },
               { "order_type": "ORDER_TYPE_DINE_IN", "base_order_type": "ORDER_TYPE_DINE_IN" }]),
        json!([{ "order_type": "ORDER_TYPE_DINE_IN", "base_order_type": "ORDER_TYPE_DINE_IN", "default": true },
               { "order_type": "ORDER_TYPE_TAKEAWAY", "base_order_type": "ORDER_TYPE_TAKEAWAY", "default": true }]),
        json!([{ "order_type": "ORDER_TYPE_DINE_IN", "base_order_type": "ORDER_TYPE_DINE_IN",
                 "default": true, "active": false }]),
        json!([{ "order_type": "ORDER_TYPE_DINE_IN", "base_order_type": "ORDER_TYPE_DINE_IN",
                 "applies_to_sources": ["SALE_SOURCE_POS"] }]),
        json!([{ "order_type": "ORDER_TYPE_DINE_IN", "base_order_type": "ORDER_TYPE_DINE_IN",
                 "price": 1 }]),
        json!([{ "order_type": "ORDER_TYPE_DINE_IN", "base_order_type": "ORDER_TYPE_DINE_IN",
                 "names": { "EN": "Eat in" } }]),
        json!([{ "order_type": "ORDER_TYPE_DINE_IN", "base_order_type": "ORDER_TYPE_DINE_IN",
                 "names": { "en": "   " } }]),
    ];
    for value in refused {
        assert_eq!(
            check_order_types(&value),
            Err(ListRefusal::NotAList),
            "{value}"
        );
    }
    for value in [
        json!([{ "source": "SALE_SOURCE_APP" }]),
        json!([{ "source": "SALE_SOURCE_HOTLINE" }]),
        json!([{ "source": "ORDER_TYPE_PICKUP", "names": { "en": "Pick-up" } }]),
    ] {
        assert_eq!(check_sources(&value), Err(ListRefusal::NotAList), "{value}");
    }
}

#[test]
fn the_cloud_holds_a_list_to_its_limits() {
    let types: Vec<_> = (0..=super::MAX_ORDER_TYPES)
        .map(|index| {
            json!({ "order_type": format!("ORDER_TYPE_T{index}"),
                    "base_order_type": "ORDER_TYPE_TAKEAWAY", "names": { "en": "T" } })
        })
        .collect();
    assert_eq!(
        check_order_types(&json!(types)),
        Err(ListRefusal::OverLimit)
    );
    let sources: Vec<_> = (0..=super::MAX_SOURCES)
        .map(|index| json!({ "source": format!("SALE_SOURCE_S{index}"), "names": { "en": "S" } }))
        .collect();
    assert_eq!(check_sources(&json!(sources)), Err(ListRefusal::OverLimit));
    let long = "x".repeat(super::MAX_NAME_CHARS + 1);
    assert_eq!(
        check_sources(&json!([{ "source": "SALE_SOURCE_LONG", "names": { "en": long } }])),
        Err(ListRefusal::OverLimit)
    );
    let languages = json!({ "en": "a", "vi": "b", "ja": "c", "ko": "d", "fr": "e" });
    assert_eq!(
        check_sources(&json!([{ "source": "SALE_SOURCE_MANY", "names": languages }])),
        Err(ListRefusal::OverLimit)
    );
}

#[test]
fn a_lists_tokens_are_read_for_the_never_dropped_rule() {
    let store = pickup_store();
    assert_eq!(
        tokens_of(&field(&store, "sources"), "source"),
        [
            "SALE_SOURCE_HOTLINE",
            "SALE_SOURCE_KIOSK",
            "SALE_SOURCE_WEBSITE"
        ]
    );
    assert!(tokens_of(&json!(null), "source").is_empty());
}
