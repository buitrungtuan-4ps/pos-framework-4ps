// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use serde_json::{Value, json};

use crate::REQUEST_BYTES_MAX;
use crate::abi::call;
use crate::engine::Engine;

/// The guest count before the first line, as `pos-proto`'s own test writes it.
fn dine_in() -> Value {
    json!({
        "flow_id": "dine_in",
        "start_action_id": "sales.table.seat",
        "steps": [
            {
                "step_id": "guests",
                "action_id": "sales.table.seat",
                "kind": "STEP_KIND_REQUIRED",
                "applies_when": "channel == 'SALES_CHANNEL_DINE_IN'",
                "done_when": "has(order.guest_count) && order.guest_count > 0"
            },
            {
                "step_id": "first_line",
                "action_id": "sales.line.add",
                "kind": "STEP_KIND_REQUIRED",
                "prerequisites": ["guests"]
            }
        ]
    })
}

/// Whether an answer says `ok`.
fn ok(answer: &Value) -> Option<bool> {
    answer.get("ok").and_then(Value::as_bool)
}

fn ask(engine: &mut Engine, request: &Value) -> Value {
    let answer = engine.answer(&serde_json::to_vec(request).expect("a request serialises"));
    serde_json::from_slice(&answer).expect("every answer is JSON")
}

fn loaded() -> Engine {
    let mut engine = Engine::default();
    let answer = ask(&mut engine, &json!({ "op": "load", "flows": [dine_in()] }));
    assert_eq!(answer, json!({ "ok": true, "flows": 1, "refused": [] }));
    engine
}

#[test]
fn a_dine_in_order_offers_the_guest_count_first() {
    let mut engine = loaded();
    let facts = json!({ "channel": "SALES_CHANNEL_DINE_IN", "order": { "line_count": 0 } });
    let answer = ask(
        &mut engine,
        &json!({ "op": "available", "flow_id": "dine_in", "facts": facts }),
    );
    assert_eq!(answer, json!({ "ok": true, "steps": ["guests"] }));
    let answer = ask(
        &mut engine,
        &json!({ "op": "missing", "flow_id": "dine_in", "step_id": "first_line", "facts": facts }),
    );
    assert_eq!(answer, json!({ "ok": true, "missing": "guests" }));
}

#[test]
fn a_step_done_or_not_applying_lets_the_next_one_run() {
    let mut engine = loaded();
    let done = ask(
        &mut engine,
        &json!({
            "op": "missing", "flow_id": "dine_in", "step_id": "first_line",
            "progress": { "done": ["guests"] },
            "facts": { "channel": "SALES_CHANNEL_DINE_IN" }
        }),
    );
    assert_eq!(done, json!({ "ok": true, "missing": null }));
    let takeaway = ask(
        &mut engine,
        &json!({
            "op": "available", "flow_id": "dine_in",
            "facts": { "channel": "SALES_CHANNEL_TAKEAWAY" }
        }),
    );
    assert_eq!(takeaway, json!({ "ok": true, "steps": ["first_line"] }));
}

#[test]
fn a_flow_is_asked_by_its_variant() {
    let mut takeaway = dine_in();
    if let Some(applies) = takeaway.pointer_mut("/steps/0/applies_when") {
        *applies = json!("false");
    }
    let mut engine = Engine::default();
    let loaded = ask(
        &mut engine,
        &json!({ "op": "load", "flows": [dine_in(), takeaway] }),
    );
    assert_eq!(ok(&loaded), Some(true));
    let ask_variant = |engine: &mut Engine, variant: usize| {
        ask(
            engine,
            &json!({
                "op": "available", "flow_id": "dine_in", "variant": variant,
                "facts": { "channel": "SALES_CHANNEL_DINE_IN" }
            }),
        )
    };
    assert_eq!(
        ask_variant(&mut engine, 0),
        json!({ "ok": true, "steps": ["guests"] })
    );
    assert_eq!(
        ask_variant(&mut engine, 1),
        json!({ "ok": true, "steps": ["first_line"] })
    );
    assert_eq!(ok(&ask_variant(&mut engine, 2)), Some(false));
}

#[test]
fn a_step_whose_done_when_holds_is_done() {
    let mut engine = loaded();
    let answer = ask(
        &mut engine,
        &json!({
            "op": "missing", "flow_id": "dine_in", "step_id": "first_line",
            "facts": { "channel": "SALES_CHANNEL_DINE_IN", "order": { "guest_count": 4 } }
        }),
    );
    assert_eq!(answer, json!({ "ok": true, "missing": null }));
}

#[test]
fn a_condition_is_answered_as_on_the_edge() {
    let mut engine = loaded();
    let holds = |engine: &mut Engine, count: i64| {
        ask(
            engine,
            &json!({
                "op": "holds",
                "condition": "has(order.guest_count) && order.guest_count > 0",
                "facts": { "order": { "guest_count": count } }
            }),
        )
    };
    assert_eq!(holds(&mut engine, 2), json!({ "ok": true, "holds": true }));
    assert_eq!(holds(&mut engine, 0), json!({ "ok": true, "holds": false }));
    let missing = ask(
        &mut engine,
        &json!({ "op": "holds", "condition": "order.guest_count > 0", "facts": { "order": {} } }),
    );
    assert_eq!(ok(&missing), Some(false));
}

#[test]
fn a_condition_outside_the_dialect_is_reported_at_load() {
    let mut flow = dine_in();
    if let Some(step) = flow.pointer_mut("/steps/0/applies_when") {
        *step = json!("order.items.all(i, i.qty > 0)");
    }
    let mut engine = Engine::default();
    let answer = ask(&mut engine, &json!({ "op": "load", "flows": [flow] }));
    assert_eq!(ok(&answer), Some(true));
    assert_eq!(
        answer
            .get("refused")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );
}

#[test]
fn malformed_requests_are_answered_never_thrown() {
    let mut engine = loaded();
    for request in [
        &b"not json"[..],
        b"",
        b"{}",
        b"[]",
        br#"{"op":"launch"}"#,
        br#"{"op":"available"}"#,
        br#"{"op":"available","flow_id":"nope"}"#,
        br#"{"op":"missing","flow_id":"dine_in","step_id":"nope"}"#,
        br#"{"op":"holds","condition":"1 +","facts":{}}"#,
        br#"{"op":"holds","condition":"true","facts":{"secret":1}}"#,
        br#"{"op":"holds","condition":"order.total > 1","facts":{"order":{"total":1.5}}}"#,
        br#"{"op":"holds","condition":"true","facts":{"order":null}}"#,
        br#"{"op":"load","flows":[{"flow_id":1}]}"#,
        b"\xff\xfe\x00",
    ] {
        let answer: Value =
            serde_json::from_slice(&engine.answer(request)).expect("every answer is JSON");
        assert_eq!(
            ok(&answer),
            Some(false),
            "{}",
            String::from_utf8_lossy(request)
        );
        assert!(answer.get("error").is_some_and(Value::is_string));
    }
}

#[test]
fn a_fact_nested_too_deep_is_refused() {
    let mut engine = loaded();
    let mut deep = json!(1);
    for _ in 0..20 {
        deep = json!([deep]);
    }
    let answer = ask(
        &mut engine,
        &json!({ "op": "holds", "condition": "true", "facts": { "order": deep } }),
    );
    assert_eq!(ok(&answer), Some(false));
}

#[test]
fn an_oversized_request_is_refused_before_it_is_read() {
    let mut engine = loaded();
    let oversized = vec![b' '; REQUEST_BYTES_MAX + 1];
    let answer: Value =
        serde_json::from_slice(&engine.answer(&oversized)).expect("every answer is JSON");
    assert_eq!(ok(&answer), Some(false));
    // Through the exported functions too: no buffer is reserved for it.
    assert_eq!(crate::abi::till_rules_input(REQUEST_BYTES_MAX + 1), 0);
    let answer: Value = serde_json::from_slice(&call(&oversized)).expect("every answer is JSON");
    assert_eq!(ok(&answer), Some(false));
}

#[test]
fn a_length_past_the_buffer_is_an_error() {
    crate::abi::till_rules_input(4);
    let len = crate::abi::till_rules_call(5);
    assert!(len > 0);
    let answer = call(br#"{"op":"holds","condition":"true"}"#);
    let answer: Value = serde_json::from_slice(&answer).expect("every answer is JSON");
    assert_eq!(answer, json!({ "ok": true, "holds": true }));
}
