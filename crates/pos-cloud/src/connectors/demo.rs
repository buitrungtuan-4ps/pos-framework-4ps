// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The demo delivery vendor: a sandbox connector with a made-up wire format, so the connector
//! framework is proved end to end without any real marketplace's account, format or data
//! (roadmap-v3 B9.4's sample connector).
//!
//! # Its format
//!
//! The body is JSON:
//!
//! ```json
//! { "order_ref": "D-1001", "placed_at_ms": 1800000000000,
//!   "items": [{ "sku": "PIZZA-M", "quantity": 2, "note": "no basil" }],
//!   "deliver_to": { "name": "…", "phone": "…", "address": "…" } }
//! ```
//!
//! `deliver_to` is optional; when it is there, all three are.
//!
//! and the header `x-demo-signature` is `sha256=` and the lowercase hex HMAC-SHA256 of the raw body
//! under the connection's `webhook_secret`.

use hmac::digest::KeyInit as _;
use hmac::{Hmac, Mac};
use pos_providers::{Family, FieldKind, ProviderDescriptor, SettingField};
use serde::Deserialize;
use sha2::Sha256;

use super::answer::{AnswerCall, AnswerKind, DeliveryAnswer};
use super::{OrderConnector, VendorLine, VendorOrder, VendorRecipient};
use crate::archive::hex_bytes;

/// The connection setting that holds the item map, one `code=menu_item_id` per line.
pub const ITEM_MAP: &str = "item_map";
/// The connection secret the vendor signs its webhooks with.
pub const WEBHOOK_SECRET: &str = "webhook_secret";
/// The header the demo vendor signs in.
pub const SIGNATURE_HEADER: &str = "x-demo-signature";

/// The demo vendor in the cloud's provider catalogue: a sandbox, serving every country.
pub static PROVIDER: ProviderDescriptor = ProviderDescriptor {
    provider_id: "delivery.demo",
    family: Family::Delivery,
    name_key: "provider.delivery.demo",
    countries: &[],
    fields: &[
        SettingField {
            key: ITEM_MAP,
            label_key: "provider.field.item_map",
            kind: FieldKind::Text { max_len: 64_000 },
            required: true,
        },
        SettingField {
            key: super::answer::ANSWER_URL,
            label_key: "provider.field.answer_url",
            kind: FieldKind::Url,
            required: false,
        },
        SettingField {
            key: super::vendors::STAFF_CONFIRM,
            label_key: "provider.field.staff_confirm",
            kind: FieldKind::Flag,
            required: true,
        },
        SettingField {
            key: super::vendors::PREP_MINUTES,
            label_key: "provider.field.prep_minutes",
            kind: FieldKind::Number {
                min: pos_proto::vendors::MIN_PREP_MINUTES as i64,
                max: pos_proto::vendors::MAX_PREP_MINUTES as i64,
            },
            required: true,
        },
        SettingField {
            key: WEBHOOK_SECRET,
            label_key: "provider.field.webhook_secret",
            kind: FieldKind::Secret { max_len: 128 },
            required: true,
        },
    ],
    capabilities: &[],
    sandbox: true,
};

/// The demo vendor's connector.
#[derive(Clone, Copy, Debug, Default)]
pub struct DemoConnector;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DemoOrder {
    order_ref: String,
    placed_at_ms: i64,
    items: Vec<DemoItem>,
    #[serde(default)]
    deliver_to: Option<DemoRecipient>,
}

/// Who the demo vendor's order goes to.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DemoRecipient {
    name: String,
    phone: String,
    address: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DemoItem {
    sku: String,
    quantity: u32,
    #[serde(default)]
    note: Option<String>,
}

impl OrderConnector for DemoConnector {
    fn provider_id(&self) -> &'static str {
        PROVIDER.provider_id
    }

    fn verify(&self, headers: &axum::http::HeaderMap, body: &[u8], secret: &str) -> bool {
        let Some(signature) = headers
            .get(SIGNATURE_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("sha256="))
            .and_then(hex_bytes)
        else {
            return false;
        };
        let Ok(mut mac) = <Hmac<Sha256>>::new_from_slice(secret.as_bytes()) else {
            return false;
        };
        mac.update(body);
        mac.verify_slice(&signature).is_ok()
    }

    fn read(&self, body: &[u8]) -> Result<VendorOrder, &'static str> {
        let order: DemoOrder =
            serde_json::from_slice(body).map_err(|_| "the body is not a demo vendor order")?;
        Ok(VendorOrder {
            reference: order.order_ref,
            placed_at_ms: order.placed_at_ms,
            lines: order
                .items
                .into_iter()
                .map(|item| VendorLine {
                    item_code: item.sku,
                    quantity: item.quantity,
                    note: item.note,
                })
                .collect(),
            recipient: order.deliver_to.map(|to| VendorRecipient {
                name: to.name,
                phone: to.phone,
                address: to.address,
            }),
        })
    }

    fn answer(&self, reference: &str, answer: &DeliveryAnswer, secret: &str) -> Option<AnswerCall> {
        demo_answer(reference, answer, secret)
    }
}

/// The demo vendor's answer body: `{"order_ref", "answer", "prep_minutes"?, "reason"?}`, the
/// answer in lower case, signed in [`SIGNATURE_HEADER`] as its webhooks are.
fn demo_answer(reference: &str, answer: &DeliveryAnswer, secret: &str) -> Option<AnswerCall> {
    let word = match answer.kind {
        AnswerKind::Accept => "accept",
        AnswerKind::Reject => "reject",
        AnswerKind::Ready => "ready",
        AnswerKind::Busy => "busy",
    };
    let body = serde_json::to_vec(&serde_json::json!({
        "order_ref": reference,
        "answer": word,
        "prep_minutes": answer.prep_minutes,
        "reason": answer.reason_code_id,
    }))
    .ok()?;
    let signature = sign(&body, secret)?;
    Some(AnswerCall {
        headers: vec![(SIGNATURE_HEADER.to_owned(), signature)],
        body,
    })
}

/// Signs `body` as the demo vendor does, for tests and a sample request. `None` only if HMAC refused
/// the key, which it never does for any length.
#[must_use]
pub fn sign(body: &[u8], secret: &str) -> Option<String> {
    let mut mac = <Hmac<Sha256>>::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(body);
    Some(format!(
        "sha256={}",
        crate::archive::to_hex(&mac.finalize().into_bytes())
    ))
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    use super::{DemoConnector, PROVIDER, SIGNATURE_HEADER, sign};
    use crate::connectors::OrderConnector as _;

    const BODY: &[u8] = br#"{"order_ref":"D-1001","placed_at_ms":1800000000000,"items":[{"sku":"PIZZA-M","quantity":2,"note":"no basil"}]}"#;

    fn signed(signature: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            SIGNATURE_HEADER,
            HeaderValue::from_str(signature).expect("a header value"),
        );
        headers
    }

    #[test]
    fn only_the_vendor_s_own_signature_over_the_exact_body_is_accepted() {
        let connector = DemoConnector;
        assert!(connector.verify(
            &signed(&sign(BODY, "demo-secret").expect("signs")),
            BODY,
            "demo-secret"
        ));
        assert!(!connector.verify(
            &signed(&sign(BODY, "other").expect("signs")),
            BODY,
            "demo-secret"
        ));
        assert!(!connector.verify(
            &signed(&sign(b"{}", "demo-secret").expect("signs")),
            BODY,
            "demo-secret"
        ));
        assert!(!connector.verify(&signed("sha256=zz"), BODY, "demo-secret"));
        assert!(!connector.verify(&HeaderMap::new(), BODY, "demo-secret"));
    }

    #[test]
    fn the_demo_order_reads_into_a_vendor_order() {
        let order = DemoConnector.read(BODY).expect("reads");
        assert_eq!(order.reference, "D-1001");
        assert_eq!(order.lines[0].item_code, "PIZZA-M");
        assert_eq!(order.lines[0].quantity, 2);
        assert!(DemoConnector.read(br#"{"order_ref":"x"}"#).is_err());
        assert!(order.recipient.is_none());
        assert!(
            DemoConnector
                .read(br#"{"order_ref":"x","placed_at_ms":1,"items":[],"address":"1 Street"}"#)
                .is_err(),
            "a field the format does not have is refused"
        );
    }

    #[test]
    fn the_demo_order_names_its_recipient_under_deliver_to() {
        let order = DemoConnector
            .read(
                br#"{"order_ref":"D-1","placed_at_ms":1,"items":[],"deliver_to":{"name":"Test Guest","phone":"000-0000","address":"1 Example Street"}}"#,
            )
            .expect("reads");
        let recipient = order.recipient.expect("a recipient");
        assert_eq!(recipient.address, "1 Example Street");
        assert_eq!(format!("{recipient:?}"), "VendorRecipient([REDACTED])");
        assert!(
            DemoConnector
                .read(br#"{"order_ref":"D-1","placed_at_ms":1,"items":[],"deliver_to":{"address":"x"}}"#)
                .is_err(),
            "a recipient names all three"
        );
    }

    #[test]
    fn the_descriptor_is_a_sandbox_the_catalogue_accepts() {
        assert!(PROVIDER.sandbox);
        pos_providers::Registry::new([&PROVIDER]).expect("a well-formed descriptor");
    }
}
