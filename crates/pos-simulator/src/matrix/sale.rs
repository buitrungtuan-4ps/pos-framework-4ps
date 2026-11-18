// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! One cell's sale, over the edge's routes as the till makes it: open a shift, order, pay, close.

use pos_proto::money::Money;
use serde_json::{Value, json};

use super::store::Till;

/// What the sale came to.
pub(super) struct Sale {
    pub(super) receipt_number: u64,
    pub(super) paid: Money,
    pub(super) expected_cash: Money,
}

/// A step that failed, and why.
type Failed = (&'static str, String);

/// Opens a shift, sells two lines, pays the bill in cash and closes the shift.
pub(super) async fn sell(till: &Till) -> Result<Sale, Failed> {
    let menu = till
        .send("GET", "/api/menu", None)
        .await
        .map_err(|detail| ("read the menu", detail))?;
    // What a till can ring up: an item the store has a tax rate for. A pack may leave a class
    // unpriced on purpose (India's alcohol is outside GST, and each state sets its own), and a
    // store there publishes its rate before it sells one, so the sale takes the priced items.
    let items: Vec<Value> = menu["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| !item["tax_rate"].is_null() && item["available"] != false)
        .take(2)
        .cloned()
        .collect();
    if items.len() < 2 {
        return Err((
            "read the menu",
            format!("{} item(s) the store can sell: {menu}", items.len()),
        ));
    }

    let zero = Money::zero(till.currency);
    let shift = till
        .send(
            "POST",
            "/api/shifts",
            Some(json!({ "opening_float": zero })),
        )
        .await
        .map_err(|detail| ("open the shift", detail))?;
    let shift_id = text(&shift, "shift_id").map_err(|detail| ("open the shift", detail))?;

    let bill_id = if till.tables {
        at_a_table(till, &items).await?
    } else {
        at_the_counter(till, &items).await?
    };

    let check = till
        .send("GET", &format!("/api/bills/{bill_id}/check"), None)
        .await
        .map_err(|detail| ("read the check", detail))?;
    let paid: Money = serde_json::from_value(check["total_due"].clone())
        .map_err(|error| ("read the check", format!("no total due: {error}: {check}")))?;
    if paid.currency_code != till.currency || paid.amount_minor <= 0 {
        return Err(("read the check", format!("total due {check}")));
    }
    let settled = till
        .send(
            "POST",
            &format!("/api/bills/{bill_id}/settle"),
            Some(json!({ "payments": [{
                "method": "PAYMENT_METHOD_CASH",
                "tendered": paid,
                "applied_to_bill": paid,
            }] })),
        )
        .await
        .map_err(|detail| ("pay the bill", detail))?;
    if settled["state"] != "BILL_STATE_SETTLED" {
        return Err(("pay the bill", format!("not settled: {settled}")));
    }
    let receipt_number = settled["receipt_number"].as_u64().unwrap_or_default();
    if receipt_number != 1 {
        return Err(("pay the bill", format!("receipt number {settled}")));
    }

    let expected_cash = close_shift(till, &shift_id, paid).await?;
    Ok(Sale {
        receipt_number,
        paid,
        expected_cash,
    })
}

/// Seats the first table on the floor, adds two lines priced from the menu as the till prices
/// them, sends them to the kitchen and opens the bill; the bill's id.
async fn at_a_table(till: &Till, items: &[Value]) -> Result<String, Failed> {
    let floor = till
        .send("GET", "/api/floor", None)
        .await
        .map_err(|detail| ("read the floor", detail))?;
    let table_id = floor["floor"]["areas"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|area| area["tables"].as_array().cloned().unwrap_or_default())
        .find_map(|table| table["table_id"].as_str().map(str::to_owned))
        .ok_or_else(|| ("read the floor", format!("no table: {floor}")))?;
    till.send("POST", &format!("/api/tables/{table_id}/seat"), None)
        .await
        .map_err(|detail| ("seat the table", detail))?;
    let mut order_id = String::new();
    for (item, quantity) in items.iter().zip([1_i64, 2]) {
        let unit = &item["unit_price"];
        let total = json!({
            "currency_code": unit["currency_code"],
            "amount_minor": unit["amount_minor"].as_i64().unwrap_or_default() * quantity,
        });
        let line = till
            .send(
                "POST",
                &format!("/api/tables/{table_id}/lines"),
                Some(json!({
                    "menu_item_id": item["menu_item_id"],
                    "display_name": item["display_name"],
                    "quantity": { "milli": quantity * 1_000 },
                    "unit_price": unit,
                    "line_total": total,
                    "tax_class_id": item["tax_class_id"],
                    "tax_rate": item["tax_rate"],
                    "note_present": false,
                })),
            )
            .await
            .map_err(|detail| ("add a line", detail))?;
        order_id = text(&line, "order_id").map_err(|detail| ("add a line", detail))?;
    }
    if till.kitchen {
        fire(till, &order_id).await?;
    }
    let bill = till
        .send("POST", &format!("/api/tables/{table_id}/bill"), None)
        .await
        .map_err(|detail| ("open the bill", detail))?;
    let bill_id = text(&bill, "bill_id").map_err(|detail| ("open the bill", detail))?;
    // The table is cleaned after it pays; checked here by its state on the bill's answer.
    if bill["table_state"] != "TABLE_STATE_AWAITING_PAYMENT" {
        return Err((
            "open the bill",
            format!("table not awaiting payment: {bill}"),
        ));
    }
    Ok(bill_id)
}

/// Opens a takeaway order at the counter, adds two lines the edge prices, sends them to the
/// kitchen where there is one and opens the bill; the bill's id.
async fn at_the_counter(till: &Till, items: &[Value]) -> Result<String, Failed> {
    let opened = till
        .send(
            "POST",
            "/api/orders",
            Some(json!({ "channel": "SALES_CHANNEL_TAKEAWAY" })),
        )
        .await
        .map_err(|detail| ("open a counter order", detail))?;
    let order_id = text(&opened, "order_id").map_err(|detail| ("open a counter order", detail))?;
    for (item, quantity) in items.iter().zip([1_i64, 2]) {
        till.send(
            "POST",
            &format!("/api/orders/{order_id}/lines"),
            Some(json!({
                "menu_item_id": item["menu_item_id"],
                "quantity": { "milli": quantity * 1_000 },
            })),
        )
        .await
        .map_err(|detail| ("add a line", detail))?;
    }
    if till.kitchen {
        fire(till, &order_id).await?;
    }
    let bill = till
        .send("POST", &format!("/api/orders/{order_id}/bill"), None)
        .await
        .map_err(|detail| ("open the bill", detail))?;
    text(&bill, "bill_id").map_err(|detail| ("open the bill", detail))
}

/// Sends every unsent line of the order to the kitchen.
async fn fire(till: &Till, order_id: &str) -> Result<(), Failed> {
    till.send(
        "POST",
        &format!("/api/orders/{order_id}/fire"),
        Some(json!({})),
    )
    .await
    .map(|_| ())
    .map_err(|detail| ("send to the kitchen", detail))
}

/// Counts the drawer at what it should hold and closes the shift; what the edge expected, which
/// must be what was paid, with no variance.
async fn close_shift(till: &Till, shift_id: &str, paid: Money) -> Result<Money, Failed> {
    till.send(
        "POST",
        &format!("/api/shifts/{shift_id}/count"),
        Some(json!({ "counted_minor": paid.amount_minor })),
    )
    .await
    .map_err(|detail| ("count the drawer", detail))?;
    let closed = till
        .send("POST", &format!("/api/shifts/{shift_id}/close"), None)
        .await
        .map_err(|detail| ("close the shift", detail))?;
    let expected: Money = serde_json::from_value(closed["expected_amount"].clone())
        .map_err(|error| ("close the shift", format!("{error}: {closed}")))?;
    let variance: Money = serde_json::from_value(closed["variance"].clone())
        .map_err(|error| ("close the shift", format!("{error}: {closed}")))?;
    if expected != paid || !variance.is_zero() {
        return Err((
            "close the shift",
            format!("the drawer does not reconcile with the sale: {closed}"),
        ));
    }
    Ok(expected)
}

/// A string field of an answer.
fn text(answer: &Value, field: &str) -> Result<String, String> {
    answer[field]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("no {field}: {answer}"))
}
