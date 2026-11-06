// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use serde_json::json;

use super::{
    CHANNEL_TEMPLATES_MAX, CUSTOM_FIELDS_MAX, KEYS_MAX, NODE_BYTES_MAX, PersonCopy,
    PublishedReceipt, ReceiptSection, ReceiptSections, TemplateRefusal, check_templates,
};
use crate::enums::SalesChannel;
use crate::wire_enum::WireEnum;

const FIELD: &str = "01J0000000000000000000CF01";

#[test]
fn a_node_without_templates_prints_as_before() {
    let node: PublishedReceipt = serde_json::from_str("{}").expect("an empty node parses");
    assert_eq!(node, PublishedReceipt::default());
    for channel in SalesChannel::ALL {
        assert!(node.template_for(*channel).is_none(), "{channel}");
    }
    assert_eq!(serde_json::to_string(&node).expect("serialise"), "{}");
}

#[test]
fn an_order_takes_its_channels_template_then_the_base() {
    let node: PublishedReceipt = serde_json::from_value(json!({
        "templates": {
            "base": { "footer_keys": ["receipt.footer.thanks"] },
            "channels": {
                "SALES_CHANNEL_TAKEAWAY": { "footer_keys": ["receipt.footer.takeaway"] }
            }
        }
    }))
    .expect("the node parses");
    let footer = |channel| {
        node.template_for(channel)
            .and_then(|template| template.footer_keys.first())
            .map(|key| key.as_str().to_owned())
    };
    assert_eq!(
        footer(SalesChannel::Takeaway).as_deref(),
        Some("receipt.footer.takeaway")
    );
    assert_eq!(
        footer(SalesChannel::DineIn).as_deref(),
        Some("receipt.footer.thanks")
    );
    // A channel from a newer release reads as UNSPECIFIED here, and takes the base.
    assert_eq!(
        footer(SalesChannel::Unspecified).as_deref(),
        Some("receipt.footer.thanks")
    );
}

#[test]
fn a_channel_with_no_template_and_no_base_prints_as_before() {
    let node: PublishedReceipt = serde_json::from_value(json!({
        "templates": { "channels": { "SALES_CHANNEL_QR": {} } }
    }))
    .expect("the node parses");
    assert!(node.template_for(SalesChannel::DineIn).is_none());
    assert!(node.template_for(SalesChannel::Qr).is_some());
}

#[test]
fn every_section_prints_until_a_template_turns_it_off() {
    let all_on = ReceiptSections::default();
    for section in ReceiptSection::ALL {
        assert!(all_on.is_on(*section), "{section}");
    }
    assert!(all_on.turned_off().is_empty());
    let some_off = ReceiptSections {
        payments: Some(false),
        change_line: Some(false),
        modifiers: Some(true),
        second_language: None,
    };
    assert_eq!(
        some_off.turned_off(),
        vec![ReceiptSection::Payments, ReceiptSection::ChangeLine]
    );
}

#[test]
fn only_a_known_document_the_template_names_is_the_persons_copy() {
    let node: PublishedReceipt = serde_json::from_value(json!({
        "templates": { "base": { "person_copy": ["PERSON_COPY_RECORD_SLIP", "PERSON_COPY_GUEST_PHONE"] } }
    }))
    .expect("a token from a newer release still parses");
    let template = node
        .template_for(SalesChannel::DineIn)
        .expect("the base template");
    assert!(template.is_person_copy(PersonCopy::RecordSlip));
    assert!(!template.is_person_copy(PersonCopy::TaxInvoiceBuyer));
    assert!(
        !template.is_person_copy(PersonCopy::Unspecified),
        "a newer token is no document this release prints"
    );
}

#[test]
fn a_template_round_trips_and_leaves_what_it_does_not_set_off_the_wire() {
    let value = json!({
        "base": {
            "header_keys": ["receipt.header.welcome"],
            "sections": { "payments": false },
            "custom_field_ids": [FIELD],
            "person_copy": ["PERSON_COPY_TAX_INVOICE_BUYER"]
        },
        "channels": { "SALES_CHANNEL_DELIVERY": {} }
    });
    let templates = check_templates(&value).expect("a valid value");
    assert_eq!(serde_json::to_value(&templates).expect("serialise"), value);
}

#[test]
fn the_write_check_takes_an_empty_value_and_a_full_one() {
    assert!(check_templates(&json!({})).is_ok());
    let keys: Vec<String> = (0..KEYS_MAX).map(|n| format!("receipt.line.{n}")).collect();
    let fields: Vec<String> = (0..CUSTOM_FIELDS_MAX)
        .map(|n| format!("01J0000000000000000000{n:04}"))
        .collect();
    let value = json!({
        "base": { "header_keys": keys, "footer_keys": keys, "custom_field_ids": fields },
        "channels": {
            "SALES_CHANNEL_DINE_IN": {}, "SALES_CHANNEL_TAKEAWAY": {}, "SALES_CHANNEL_DELIVERY": {},
            "SALES_CHANNEL_QR": {}, "SALES_CHANNEL_API": {}
        }
    });
    assert!(check_templates(&value).is_ok());
}

#[test]
fn the_write_check_refuses_a_shape_that_is_not_a_template() {
    for value in [
        json!(null),
        json!("receipt"),
        json!({ "footer": "Thank you" }),
        json!({ "base": { "footer_text": "Thank you" } }),
        json!({ "base": { "header_keys": "receipt.header" } }),
        json!({ "base": { "header_keys": [""] } }),
        json!({ "base": { "header_keys": [7] } }),
        json!({ "base": { "sections": { "payments": "no" } } }),
        json!({ "base": { "sections": { "seller": false } } }),
        json!({ "base": { "sections": { "tax_lines": false } } }),
        json!({ "base": { "custom_field_ids": ["not-an-id"] } }),
        json!({ "base": { "custom_field_ids": [FIELD, FIELD] } }),
        json!({ "base": { "person_copy": ["PERSON_COPY_STORE_COPY"] } }),
        json!({ "base": { "person_copy": ["PERSON_COPY_UNSPECIFIED"] } }),
        json!({ "base": { "person_copy": ["PERSON_COPY_RECORD_SLIP", "PERSON_COPY_RECORD_SLIP"] } }),
        json!({ "channels": { "SALES_CHANNEL_KIOSK": {} } }),
        json!({ "channels": { "SALES_CHANNEL_UNSPECIFIED": {} } }),
        json!({ "channels": [] }),
    ] {
        assert_eq!(
            check_templates(&value),
            Err(TemplateRefusal::NotATemplate),
            "{value}"
        );
    }
}

#[test]
fn the_write_check_refuses_a_value_over_the_limits() {
    let keys: Vec<String> = (0..=KEYS_MAX)
        .map(|n| format!("receipt.line.{n}"))
        .collect();
    let fields: Vec<String> = (0..=CUSTOM_FIELDS_MAX)
        .map(|n| format!("01J0000000000000000000{n:04}"))
        .collect();
    let channels: serde_json::Map<String, serde_json::Value> = (0..=CHANNEL_TEMPLATES_MAX)
        .map(|n| (format!("SALES_CHANNEL_{n}"), json!({})))
        .collect();
    let long_key = "k".repeat(NODE_BYTES_MAX);
    for value in [
        json!({ "base": { "header_keys": keys } }),
        json!({ "base": { "footer_keys": keys } }),
        json!({ "base": { "custom_field_ids": fields } }),
        json!({ "channels": channels }),
        json!({ "base": { "header_keys": [long_key] } }),
    ] {
        assert_eq!(
            check_templates(&value),
            Err(TemplateRefusal::OverLimit),
            "{value}"
        );
    }
}
