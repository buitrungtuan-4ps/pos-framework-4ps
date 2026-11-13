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
//!   "items": [{ "sku": "PIZZA-M", "quantity": 2, "note": "no basil" }] }
//! ```
//!
//! and the header `x-demo-signature` is `sha256=` and the lowercase hex HMAC-SHA256 of the raw body
//! under the connection's `webhook_secret`.

use hmac::digest::KeyInit as _;
use hmac::{Hmac, Mac};
use pos_providers::{Family, FieldKind, ProviderDescriptor, SettingField};
use serde::Deserialize;
use sha2::Sha256;

use super::{OrderConnector, VendorLine, VendorOrder};
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
        })
    }
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
        assert!(
            DemoConnector
                .read(br#"{"order_ref":"x","placed_at_ms":1,"items":[],"address":"1 Street"}"#)
                .is_err(),
            "a field the format does not have, an address among them, is refused"
        );
    }

    #[test]
    fn the_descriptor_is_a_sandbox_the_catalogue_accepts() {
        assert!(PROVIDER.sandbox);
        pos_providers::Registry::new([&PROVIDER]).expect("a well-formed descriptor");
    }
}
