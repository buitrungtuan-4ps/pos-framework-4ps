// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Every condition the console's rule builder writes compiles in this dialect and answers as the
//! builder means it ([ADR-0193](../../../docs/adr/0193-conditions-and-reactions-are-written-in-cel.md)).
//!
//! The builder (`dashboard/src/lib/rule-builder.ts`) writes CEL so nobody types it, and the cloud
//! refuses at publish what this crate cannot compile. These vectors are where the two meet: the
//! console's own test holds that the builder writes each line's condition, and this one holds that
//! the evaluator the cloud, the edge and the till run compiles it and answers it over that line's
//! facts.

use std::collections::BTreeMap;

use pos_rules::{Fact, Facts, Rules, Variable};

/// The console's file, read here so the two cannot drift: `xtask`'s `READ_BY` lists it for this crate.
const VECTORS: &str = include_str!("../../../dashboard/tests/rule-conditions.txt");

/// An order's facts as `pos-edge-app` and the till fill them, or why a vector's numbers do not
/// read.
fn facts(guest_count: &str, line_count: &str, channel: &str) -> Result<Facts, String> {
    let count = |text: &str| {
        text.parse::<i64>()
            .map_err(|error| format!("`{text}` is not a count: {error}"))
    };
    let mut order = BTreeMap::from([
        ("channel".to_owned(), Fact::Text(channel.to_owned())),
        ("line_count".to_owned(), Fact::Int(count(line_count)?)),
    ]);
    if guest_count != "-" {
        order.insert("guest_count".to_owned(), Fact::Int(count(guest_count)?));
    }
    Ok(Facts::new()
        .with(Variable::Order, Fact::Map(order))
        .with(Variable::Channel, Fact::Text(channel.to_owned())))
}

#[test]
fn every_condition_the_console_writes_compiles_and_answers_as_meant() {
    let rules = Rules::new().expect("the evaluator builds");
    let mut checked = 0;
    for line in VECTORS
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
    {
        let fields: Vec<&str> = line.splitn(5, '\t').collect();
        let [holds, guest_count, line_count, channel, condition] = fields[..] else {
            panic!("a vector has five tab-separated fields: {line}");
        };
        let compiled = rules
            .compile(condition)
            .unwrap_or_else(|error| panic!("`{condition}` compiles: {error}"));
        let facts = facts(guest_count, line_count, channel)
            .unwrap_or_else(|error| panic!("a vector's facts read: {error}"));
        let answer = compiled
            .evaluate(&facts)
            .unwrap_or_else(|error| panic!("`{condition}` answers: {error}"));
        assert_eq!(
            answer.to_string(),
            holds,
            "`{condition}` over guests {guest_count}, lines {line_count}, {channel}"
        );
        checked += 1;
    }
    assert!(checked >= 10, "the vectors are read, not skipped");
}
