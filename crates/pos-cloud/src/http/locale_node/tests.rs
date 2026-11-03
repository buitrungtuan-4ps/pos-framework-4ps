// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a store's `locale` node actually carries
//! ([ADR-0134](../../../../../docs/adr/0134-a-currency-says-how-many-decimals-it-has.md)).
//!
//! Covered here rather than through the route because `checked_locale_node` *is* what the route
//! validates: the handler around it holds a permission check, two ULID parses and a config-tree
//! write, none of which can tell you whether the node an edge receives is complete.

use crate::countries;
use crate::http::config_nodes::locale::{PublishLocaleRequest, checked_locale_node, packed_facts};
use pos_proto::locale::{PublishedLocale, PublishedNumberFormat};
use serde_json::Value;

/// A publish as the console sends one, for a store trading in `currency`.
fn request(country: &str, currency: &str) -> PublishLocaleRequest {
    PublishLocaleRequest {
        tenant_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
        store_id: "01ARZ3NDEKTSV4RRFFQ69G5FAW".to_owned(),
        country_code: country.to_owned(),
        currency_code: currency.to_owned(),
        timezone: "Asia/Kolkata".to_owned(),
        cutoff_hour: 4,
        display_language: None,
        prices_include_tax: true,
        cash_rounding_increment: Some(100),
        cash_denominations: vec![10_000, 20_000],
        cash_count_denominations: None,
    }
}

#[test]
fn a_store_that_states_no_counting_list_counts_by_its_country_s() {
    // ADR-0186: the pack states the list, and a publish that says nothing takes it.
    let node = checked_locale_node(&request("VN", "VND"), &packed_facts(&countries::registry()))
        .expect("a valid publish");
    assert_eq!(
        node.get("cash_count_denominations"),
        Some(&serde_json::json!([
            1_000, 2_000, 5_000, 10_000, 20_000, 50_000, 100_000, 200_000, 500_000
        ])),
        "{node}"
    );
}

#[test]
fn a_store_s_own_counting_list_wins_over_its_country_s_and_empty_means_none() {
    let facts = packed_facts(&countries::registry());
    let mut own = request("VN", "VND");
    own.cash_count_denominations = Some(vec![500_000, 10_000, 10_000]);
    let node = checked_locale_node(&own, &facts).expect("a valid publish");
    assert_eq!(
        node.get("cash_count_denominations"),
        Some(&serde_json::json!([10_000, 500_000])),
        "the store's list, ascending and de-duplicated: {node}"
    );

    own.cash_count_denominations = Some(Vec::new());
    let node = checked_locale_node(&own, &facts).expect("a valid publish");
    assert_eq!(
        node.get("cash_count_denominations"),
        Some(&serde_json::json!([])),
        "an empty list is the store's answer, not a gap the pack fills: {node}"
    );

    own.cash_count_denominations = Some(vec![1_000, 0]);
    assert!(
        checked_locale_node(&own, &facts).is_err(),
        "a note worth nothing is refused"
    );
}

#[test]
fn the_node_tells_the_store_how_many_decimals_its_currency_has() {
    // ADR-0134: "It rides the `locale` config node to the store and is served on
    // `GET /api/locale`." Without it the edge keeps its bootstrap zero, `GET /api/locale`
    // serves that zero as though it were published, and the till's `?? fallbackExponent(...)`
    // never fires because `0` is not nullish. An Indian store then draws 26145 paise as
    // `INR 26,145` and prints `INR 26145` on a tax invoice — the two defects #416 and #417
    // were merged to fix.
    let node = checked_locale_node(&request("IN", "INR"), &packed_facts(&countries::registry()))
        .expect("a valid publish");
    assert_eq!(
        node.get("currency_exponent").and_then(Value::as_u64),
        Some(2),
        "the rupee has two decimal places and the node has to say so: {node}"
    );
}

#[test]
fn the_node_tells_the_store_how_long_to_keep_a_personal_record() {
    // ADR-0107 gave the edge its own retention sweep and had it read this key. The node never
    // carried it, so every store ran on the bootstrap 365 instead of its country's figure.
    //
    // Nothing was wrong *yet* — all four packs say 365 — which is precisely why it needed a
    // test rather than a reader noticing. The figure each pack carries is marked "a default,
    // not a determination"; the first time one is set for real, this is what makes the publish
    // mean something.
    let node = checked_locale_node(&request("IN", "INR"), &packed_facts(&countries::registry()))
        .expect("a valid publish");
    assert_eq!(
        node.get("default_retention_days").and_then(Value::as_u64),
        Some(365),
        "the store's retention period has to reach the store: {node}"
    );
}

#[test]
fn the_node_tells_the_store_how_its_country_writes_a_number() {
    // ADR-0136. Vietnam is the case worth asserting: it is the only shipped pack whose format
    // differs from the compiled default, so it is the only one where a store receiving nothing
    // and a store receiving the right thing look different.
    let node = checked_locale_node(&request("VN", "VND"), &packed_facts(&countries::registry()))
        .expect("a valid publish");
    let format = node.get("number_format").expect("the node names a format");
    assert_eq!(
        format.get("decimal_separator").and_then(Value::as_str),
        Some(",")
    );
    assert_eq!(
        format.get("group_separator").and_then(Value::as_str),
        Some(".")
    );
    assert_eq!(
        format.get("digits_per_group").and_then(Value::as_u64),
        Some(3)
    );
}

#[test]
fn the_node_tells_the_store_the_language_of_its_country() {
    // ADR-0160: a new store's receipts print in its country's language, which the edge has no
    // pack to look up.
    let node = checked_locale_node(&request("VN", "VND"), &packed_facts(&countries::registry()))
        .expect("a valid publish");
    assert_eq!(
        node.get("country_language").and_then(Value::as_str),
        Some("vi"),
        "{node}"
    );
}

/// The node carries every field the edge reads, and nothing else but the two the cloud keeps for
/// itself.
///
/// The fields are the type's, `pos_proto::locale::PublishedLocale`, the one the edge applies the
/// node through, rather than a list written out here: the node parses as it, sends each field it
/// reads and reads back as stated, and sends no other key but the cloud's own. A field added to
/// the type does not compile below until it is stated.
///
/// Both directions are checked, because the gap has appeared both ways round: a key the edge
/// reads and the cloud never sends is a feature that does nothing, which is where
/// `currency_exponent` and `default_retention_days` both sat through the releases that depended
/// on them, and a key the cloud sends and the edge never reads is a payload nobody consumes.
/// Neither shows up as a failure anywhere else.
///
/// Its sibling closes the other half: `every_field_a_locale_node_carries_reaches_the_session`
/// in `crates/pos-edge/src/config_client.rs` asserts each of these fields actually moves a
/// session field.
#[test]
fn the_node_carries_what_the_edge_reads_and_nothing_unclaimed() {
    // Not read through `PublishedLocale`. `country_code` is the fleet console's own comparison
    // against a store's region (ADR-0114). `display_language` is the store's choice rather than
    // its country's, and the edge reads it apart from the type, field by field, to name the
    // menu's items in it and keep it on the session (ADR-0074).
    const THE_CLOUD_S_OWN: &[&str] = &["country_code", "display_language"];

    let mut published = request("VN", "VND");
    published.display_language = Some("vi".to_owned());
    let node = checked_locale_node(&published, &packed_facts(&countries::registry()))
        .expect("a valid publish");
    let sent = node.as_object().expect("the node is an object");

    // The node as the edge reads it, through its text as `session_from_config` parses it, and
    // written back: an optional field the node sends as `null` reads as absent.
    let read: PublishedLocale =
        serde_json::from_str(&node.to_string()).expect("the edge applies the node it is sent");
    let read = serde_json::to_value(read).expect("serialise");
    let read = read.as_object().expect("an object");

    // Every key the type reads, from the type: a value stating each field, written.
    let every_field = serde_json::to_value(PublishedLocale {
        currency_code: String::new(),
        currency_exponent: Some(0),
        timezone: String::new(),
        cutoff_hour: 0,
        prices_include_tax: true,
        cash_rounding_increment: Some(0),
        cash_denominations: vec![0],
        cash_count_denominations: vec![0],
        default_retention_days: Some(0),
        number_format: Some(PublishedNumberFormat {
            decimal_separator: String::new(),
            group_separator: String::new(),
            digits_per_group: 0,
        }),
        country_language: Some(String::new()),
    })
    .expect("serialise");
    let every_field = every_field.as_object().expect("an object");

    let missing: Vec<&String> = every_field
        .keys()
        .filter(|key| !sent.contains_key(*key) || !read.contains_key(*key))
        .collect();
    assert!(
        missing.is_empty(),
        "the edge parses these and the node does not send them, so they do nothing: {missing:?}"
    );

    let unclaimed: Vec<&String> = sent
        .keys()
        .filter(|key| !every_field.contains_key(*key) && !THE_CLOUD_S_OWN.contains(&key.as_str()))
        .collect();
    assert!(
        unclaimed.is_empty(),
        "the node sends these and nothing reads them — add them to `PublishedLocale` or to the \
         list above, whichever is true: {unclaimed:?}"
    );
}

#[test]
fn a_zero_decimal_currency_says_zero_rather_than_saying_nothing() {
    // The đồng's exponent really is zero, and that has to be *published* rather than left to
    // the edge's bootstrap to coincide with. The two are indistinguishable on a Vietnamese
    // store, which is exactly why the rupee case above is the one that catches this.
    let node = checked_locale_node(&request("VN", "VND"), &packed_facts(&countries::registry()))
        .expect("a valid publish");
    assert_eq!(
        node.get("currency_exponent").and_then(Value::as_u64),
        Some(0),
        "the node must state the đồng's zero, not omit it: {node}"
    );
}
