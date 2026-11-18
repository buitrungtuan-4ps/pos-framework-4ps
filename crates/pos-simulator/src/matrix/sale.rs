// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! One cell's sale, over the edge's routes as the till makes it: open a shift, order, comp a line,
//! pay, close.

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

/// Opens a shift, sells two lines, comps the first, pays the bill in cash and closes the shift.
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
    // No template asks for a check before payment (ADR-0218), and the till learns that from this read.
    if menu["check_before_payment"] != false {
        return Err((
            "read the menu",
            format!("a check before payment the template does not ask for: {menu}"),
        ));
    }
    if items.len() < 2 {
        return Err((
            "read the menu",
            format!("{} item(s) the store can sell: {menu}", items.len()),
        ));
    }
    scans(&menu, &items)?;

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
    super::tab::tab(till).await?;
    if menu["barcode_enabled"] == json!(true) {
        super::labels::labels(till, &items[0]).await?;
    }

    let (bill_id, first_line) = if till.tables() {
        at_a_table(till, &items).await?
    } else {
        at_the_counter(till, &items).await?
    };
    comp(till, &bill_id, &first_line).await?;

    // The guest is shown the check first (ADR-0218): the print is recorded whether or not a
    // printer answers, and the check read then carries it, fresh.
    till.send("POST", &format!("/api/bills/{bill_id}/check/print"), None)
        .await
        .map_err(|detail| ("print the check", detail))?;
    let check = till
        .send("GET", &format!("/api/bills/{bill_id}/check"), None)
        .await
        .map_err(|detail| ("read the check", detail))?;
    if check["check_printed"]["copy"] != 1 || check["check_printed"]["stale"] != false {
        return Err((
            "print the check",
            format!("no fresh check on the bill: {check}"),
        ));
    }
    let paid: Money = serde_json::from_value(check["total_due"].clone())
        .map_err(|error| ("read the check", format!("no total due: {error}: {check}")))?;
    if paid.currency_code != till.currency || paid.amount_minor <= 0 {
        return Err(("read the check", format!("total due {check}")));
    }
    let settled = pay(till, &bill_id, paid).await?;
    if settled["state"] != "BILL_STATE_SETTLED" {
        return Err(("pay the bill", format!("not settled: {settled}")));
    }
    let receipt_number = settled["receipt_number"].as_u64().unwrap_or_default();
    if receipt_number != 1 {
        return Err(("pay the bill", format!("receipt number {settled}")));
    }

    if till.kitchen() && till.pay_first() {
        sent_at_settle(till).await?;
    }
    let expected_cash = close_shift(till, &shift_id, paid).await?;
    Ok(Sale {
        receipt_number,
        paid,
        expected_cash,
    })
}

/// Gives the bill's first line away with the store's comp reason, a manager approving (ADR-0219):
/// the comp comes off what the bill owes and something is still owed for the second line.
async fn comp(till: &Till, bill_id: &str, order_line_id: &str) -> Result<(), Failed> {
    let reason_code_id = reason_for(till, "REASON_ACTION_COMP", "comp a line").await?;
    let (code, pin) = Till::approver();
    let comped = till
        .send(
            "POST",
            &format!("/api/bills/{bill_id}/comp"),
            Some(json!({
                "order_line_id": order_line_id,
                "reason_code_id": reason_code_id,
                "approver_code": code,
                "approver_pin": pin,
            })),
        )
        .await
        .map_err(|detail| ("comp a line", detail))?;
    let comp_total = comped["comp_total"]["amount_minor"]
        .as_i64()
        .unwrap_or_default();
    let total_due = comped["total_due"]["amount_minor"]
        .as_i64()
        .unwrap_or_default();
    if comp_total <= 0 || total_due <= 0 {
        return Err(("comp a line", format!("the comp did not land: {comped}")));
    }
    Ok(())
}

/// Pays the bill in cash (ADR-0220): in two halves where the store takes a bill in parts, the first
/// recorded with the bill still open and the second settling it; in one where it does not, once
/// the edge has refused a part. The settled bill.
async fn pay(till: &Till, bill_id: &str, due: Money) -> Result<Value, Failed> {
    let route = format!("/api/bills/{bill_id}/payments");
    let cash = |minor: i64| {
        let applied = Money::new(due.currency_code, minor);
        json!({
            "payment": {
                "method": "PAYMENT_METHOD_CASH",
                "tendered": applied,
                "applied_to_bill": applied,
            },
            "split_ways": 2,
        })
    };
    let half = due.amount_minor / 2;
    let first = till.send("POST", &route, Some(cash(half))).await;
    let last = if till.part_payments {
        let first = first.map_err(|detail| ("pay a part", detail))?;
        let rest = due.amount_minor - half;
        if first["remaining"]["amount_minor"].as_i64() != Some(rest) || !first["settled"].is_null()
        {
            return Err((
                "pay a part",
                format!("not left open owing the rest: {first}"),
            ));
        }
        listed_on_the_check(till, bill_id, half).await?;
        till.send("POST", &route, Some(cash(rest))).await
    } else {
        if !first
            .as_ref()
            .is_err_and(|detail| detail.contains("PART_PAYMENTS_OFF"))
        {
            return Err(("pay the bill", format!("a part was not refused: {first:?}")));
        }
        till.send("POST", &route, Some(cash(due.amount_minor)))
            .await
    };
    let last = last.map_err(|detail| ("pay the bill", detail))?;
    Ok(last["settled"].clone())
}

/// Asserts that every till's read of the bill lists the one part taken, and that the menu offers
/// the even split the till takes it with (ADR-0220 decision 3).
async fn listed_on_the_check(till: &Till, bill_id: &str, half: i64) -> Result<(), Failed> {
    let check = till
        .send("GET", &format!("/api/bills/{bill_id}/check"), None)
        .await
        .map_err(|detail| ("read the part paid", detail))?;
    let listed = check["payments"].as_array().is_some_and(|payments| {
        payments.len() == 1
            && payments[0]["amount"]["amount_minor"].as_i64() == Some(half)
            && payments[0]["split_ways"].as_u64() == Some(2)
    });
    let menu = till
        .send("GET", "/api/menu", None)
        .await
        .map_err(|detail| ("read the part paid", detail))?;
    if !listed || menu["part_payments"] != json!(true) {
        return Err((
            "read the part paid",
            format!("the check does not list it: {check}"),
        ));
    }
    Ok(())
}

/// Asserts that a store selling by barcode is told so, with its scan window, and that each item it
/// sells is found by its barcode, as the till's code index finds it (ADR-0222 decision 2); and that
/// any other store is told it does not sell by barcode.
fn scans(menu: &Value, items: &[Value]) -> Result<(), Failed> {
    let enabled = menu["barcode_enabled"] == json!(true);
    let window = menu["scan_keystroke_window_ms"].as_i64() == Some(50);
    let found = items.iter().all(|item| {
        let code = item["barcodes"][0].as_str();
        code.is_some_and(|code| {
            menu["items"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|entry| {
                    entry["barcodes"]
                        .as_array()
                        .is_some_and(|codes| codes.iter().any(|c| c == code))
                })
                == Some(item)
        })
    });
    if !window || (enabled && !found) {
        return Err((
            "scan the items",
            format!("not found by their barcodes: {menu}"),
        ));
    }
    Ok(())
}

/// The first reason the store publishes for `action`, as the till's picker offers it.
pub(super) async fn reason_for(
    till: &Till,
    action: &str,
    step: &'static str,
) -> Result<String, Failed> {
    let reasons = till
        .send("GET", "/api/reason-codes", None)
        .await
        .map_err(|detail| (step, detail))?;
    reasons["reasons"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|reason| {
            reason["applies_to"]
                .as_array()
                .is_some_and(|actions| actions.iter().any(|tagged| tagged == action))
        })
        .and_then(|reason| reason["reason_code_id"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| (step, format!("no reason for {action}: {reasons}")))
}

/// Overrides the line's price to twice the menu's, before the bill opens, with the store's
/// override reason and a manager approving (ADR-0219): the edge answers with the new total.
async fn reprice(till: &Till, order_line_id: &str, unit: &Value) -> Result<(), Failed> {
    let reason_code_id =
        reason_for(till, "REASON_ACTION_PRICE_OVERRIDE", "override a price").await?;
    let doubled = json!({
        "currency_code": unit["currency_code"],
        "amount_minor": unit["amount_minor"].as_i64().unwrap_or_default() * 2,
    });
    let (code, pin) = Till::approver();
    let repriced = till
        .send(
            "POST",
            &format!("/api/lines/{order_line_id}/price"),
            Some(json!({
                "unit_price": doubled,
                "reason_code_id": reason_code_id,
                "approver_code": code,
                "approver_pin": pin,
            })),
        )
        .await
        .map_err(|detail| ("override a price", detail))?;
    if repriced["unit_price"] != doubled {
        return Err(("override a price", format!("not repriced: {repriced}")));
    }
    Ok(())
}

/// Seats the first table on the floor, adds two lines priced from the menu as the till prices
/// them, sends them to the kitchen and opens the bill; the bill's id and its first line's.
async fn at_a_table(till: &Till, items: &[Value]) -> Result<(String, String), Failed> {
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
    let mut first_line = String::new();
    let mut last_line = String::new();
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
        last_line = text(&line, "order_line_id").map_err(|detail| ("add a line", detail))?;
        if first_line.is_empty() {
            first_line.clone_from(&last_line);
        }
    }
    reprice(till, &last_line, &items[1]["unit_price"]).await?;
    if till.kitchen() {
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
    Ok((bill_id, first_line))
}

/// Opens a takeaway order at the counter, adds two lines the edge prices, sends them to the
/// kitchen where there is one and opens the bill; the bill's id and its first line's.
async fn at_the_counter(till: &Till, items: &[Value]) -> Result<(String, String), Failed> {
    let opened = till
        .send(
            "POST",
            "/api/orders",
            Some(json!({ "channel": "SALES_CHANNEL_TAKEAWAY" })),
        )
        .await
        .map_err(|detail| ("open a counter order", detail))?;
    let order_id = text(&opened, "order_id").map_err(|detail| ("open a counter order", detail))?;
    let mut first_line = String::new();
    let mut last_line = String::new();
    for (item, quantity) in items.iter().zip([1_i64, 2]) {
        let line = till
            .send(
                "POST",
                &format!("/api/orders/{order_id}/lines"),
                Some(json!({
                    "menu_item_id": item["menu_item_id"],
                    "quantity": { "milli": quantity * 1_000 },
                })),
            )
            .await
            .map_err(|detail| ("add a line", detail))?;
        last_line = text(&line, "order_line_id").map_err(|detail| ("add a line", detail))?;
        if first_line.is_empty() {
            first_line.clone_from(&last_line);
        }
    }
    reprice(till, &last_line, &items[1]["unit_price"]).await?;
    // A pay-first counter sends nothing before it is paid: the settle sends it (ADR-0222).
    if till.kitchen() && !till.pay_first() {
        fire(till, &order_id).await?;
    }
    let bill = till
        .send("POST", &format!("/api/orders/{order_id}/bill"), None)
        .await
        .map_err(|detail| ("open the bill", detail))?;
    let bill_id = text(&bill, "bill_id").map_err(|detail| ("open the bill", detail))?;
    Ok((bill_id, first_line))
}

/// Checks the kitchen has the order a pay-first counter just settled, every line of it sent with the
/// payment (ADR-0222).
async fn sent_at_settle(till: &Till) -> Result<(), Failed> {
    let board = till
        .send("GET", "/api/orders/kitchen", None)
        .await
        .map_err(|detail| ("send at settle", detail))?;
    let lines: Vec<&Value> = board
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|order| order["lines"].as_array().into_iter().flatten())
        .collect();
    if lines.is_empty()
        || lines
            .iter()
            .any(|line| line["state"] != "ORDER_LINE_STATE_FIRED")
    {
        return Err((
            "send at settle",
            format!("the kitchen does not have it: {board}"),
        ));
    }
    Ok(())
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
