// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Price labels (ADR-0222 decision 3): a store whose pack seeds a layout reads a label by it and,
//! the edge being the only pricer, refuses one for a fixed-price item; a store whose pack seeds
//! none reads it as a plain code no item carries. A weighed item (ADR-0223) is never weighed on a
//! scale the store does not have, nor typed by hand where the store takes no typed weight.

use serde_json::{Value, json};

use super::store::Till;

/// Scans a price label for `item`'s PLU onto an empty counter order and checks the refusal, then
/// the weighing refusals for a weighed item of `menu`'s.
pub(super) async fn labels(
    till: &Till,
    item: &Value,
    menu: &Value,
) -> Result<(), (&'static str, String)> {
    let Some(plu) = item["plu"].as_str().and_then(|plu| plu.parse::<u32>().ok()) else {
        return Err(("scan a price label", format!("the item has no PLU: {item}")));
    };
    let opened = till
        .send(
            "POST",
            "/api/orders",
            Some(json!({ "channel": "SALES_CHANNEL_TAKEAWAY" })),
        )
        .await
        .map_err(|detail| ("scan a price label", detail))?;
    let order_id = opened["order_id"].as_str().unwrap_or_default().to_owned();
    let label = with_check(&format!("20{plu:05}01000"));
    let refused = till
        .send(
            "POST",
            &format!("/api/orders/{order_id}/scan"),
            Some(json!({ "code": label })),
        )
        .await;
    let expected = if till.price_labels {
        "PRICE_LABEL_NOT_OPEN_PRICE"
    } else {
        "UNKNOWN_CODE"
    };
    match refused {
        Err(refusal) if refusal.contains(expected) => weighs(till, &order_id, menu).await,
        other => Err((
            "scan a price label",
            format!("{label} was not refused with {expected}: {other:?}"),
        )),
    }
}

/// A matrix store has no scale, so weighing `menu`'s weighed item on one is `SCALE_ABSENT`; and
/// where the store takes no typed weight, a typed one is `MANUAL_WEIGHT_NOT_ALLOWED`.
async fn weighs(till: &Till, order_id: &str, menu: &Value) -> Result<(), (&'static str, String)> {
    let Some(weighed) = menu["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|item| item["sold_by_weight"] == json!(true))
    else {
        return Ok(());
    };
    let path = format!("/api/orders/{order_id}/weigh");
    let item_id = &weighed["menu_item_id"];
    let mut asks = vec![(json!("WEIGHT_SOURCE_SCALE"), "SCALE_ABSENT")];
    if menu["weighing_manual_entry"] != json!(true) {
        asks.push((json!("WEIGHT_SOURCE_MANUAL"), "MANUAL_WEIGHT_NOT_ALLOWED"));
    }
    for (source, expected) in asks {
        let body = json!({ "menu_item_id": item_id, "source": source, "net_grams": 375 });
        match till.send("POST", &path, Some(body)).await {
            Err(refusal) if refusal.contains(expected) => {}
            other => {
                return Err((
                    "weigh an item",
                    format!("{source} was not refused with {expected}: {other:?}"),
                ));
            }
        }
    }
    Ok(())
}

/// `body` with its GS1 check digit appended.
fn with_check(body: &str) -> String {
    let sum: u32 = body
        .bytes()
        .rev()
        .enumerate()
        .map(|(position, byte)| {
            let digit = u32::from(byte.saturating_sub(b'0'));
            if position % 2 == 0 { digit * 3 } else { digit }
        })
        .sum();
    format!("{body}{}", (10 - sum % 10) % 10)
}
