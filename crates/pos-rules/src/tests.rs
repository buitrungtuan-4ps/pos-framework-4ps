// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use std::collections::BTreeMap;

use crate::{DEPTH_MAX, Fact, Facts, NODES_MAX, RuleError, Rules, SOURCE_CHARS_MAX, Variable};

fn rules() -> Rules {
    Rules::new().expect("the evaluator builds")
}

/// An obviously fake order: amounts in minor units, no person anywhere.
fn order() -> Facts {
    let fields: BTreeMap<String, Fact> = [
        ("total_amount", Fact::Int(650_000)),
        ("discount_amount", Fact::Int(50_000)),
        ("guest_count", Fact::Int(5)),
        ("table", Fact::Text("T12".to_owned())),
        (
            "category_ids",
            Fact::List(vec![
                Fact::Text("cat-pizza".to_owned()),
                Fact::Text("cat-drinks".to_owned()),
            ]),
        ),
    ]
    .into_iter()
    .map(|(name, fact)| (name.to_owned(), fact))
    .collect();
    Facts::new().with(Variable::Order, Fact::Map(fields)).with(
        Variable::Channel,
        Fact::Text("SALES_CHANNEL_DINE_IN".to_owned()),
    )
}

fn answer(source: &str) -> Result<bool, RuleError> {
    rules().compile(source)?.evaluate(&order())
}

#[test]
fn a_guard_a_flow_needs_answers_on_integer_money() {
    assert_eq!(
        answer("order.total_amount >= 500000 && channel == 'SALES_CHANNEL_DINE_IN'"),
        Ok(true)
    );
    assert_eq!(
        answer("order.total_amount - order.discount_amount >= 700000"),
        Ok(false)
    );
    assert_eq!(answer("'cat-drinks' in order.category_ids"), Ok(true));
    assert_eq!(
        answer("size(order.category_ids) == 2 && order.table.startsWith('T')"),
        Ok(true)
    );
    assert_eq!(
        answer("order.table.endsWith('12') && order.table.contains('1')"),
        Ok(true)
    );
    assert_eq!(
        answer("has(order.guest_count) && order.guest_count > 4"),
        Ok(true)
    );
    assert_eq!(answer("!has(order.member_id)"), Ok(true));
    assert_eq!(answer("order.category_ids[0] == 'cat-pizza'"), Ok(true));
    assert_eq!(
        answer("order.guest_count % 2 == 1 && -order.guest_count < 0"),
        Ok(true)
    );
}

/// ADR-0193 decision 3: a loop macro is refused at compile time. Without the check it would
/// compile and fail only when it ran.
#[test]
fn every_loop_macro_is_refused_before_it_runs() {
    for (source, name) in [
        ("order.category_ids.all(c, c != '')", "all"),
        ("order.category_ids.exists(c, c == 'x')", "exists"),
        ("order.category_ids.exists_one(c, c == 'x')", "exists_one"),
        ("size(order.category_ids.map(c, c)) > 0", "map"),
        ("size(order.category_ids.filter(c, c == 'x')) > 0", "filter"),
    ] {
        assert_eq!(
            rules().compile(source).err(),
            Some(RuleError::LoopMacro(name.to_owned())),
            "{source}"
        );
    }
}

#[test]
fn a_function_off_the_allow_list_is_refused() {
    for (source, name) in [
        ("string(order.guest_count) == '5'", "string"),
        ("int('5') == 5", "int"),
        ("order.table.matches('T.*')", "matches"),
        ("timestamp('2026-01-01T00:00:00Z') != null", "timestamp"),
        ("order.guest_count > 4 ? true : false", "_?_:_"),
    ] {
        assert_eq!(
            rules().compile(source).err(),
            Some(RuleError::FunctionNotAllowed(name.to_owned())),
            "{source}"
        );
    }
}

/// Integers only for money: a fraction is refused however it is written, and an integer amount
/// times a fraction cannot even be spelled.
#[test]
fn a_fraction_is_refused() {
    for source in [
        "order.total_amount * 1.1 > 0",
        "double(order.total_amount) > 500000",
        "order.total_amount >= 0.5",
    ] {
        assert_eq!(
            rules().compile(source).err(),
            Some(RuleError::DoubleNotAllowed),
            "{source}"
        );
    }
}

#[test]
fn only_the_declared_views_are_in_scope() {
    assert_eq!(
        rules().compile("customer.phone != ''").err(),
        Some(RuleError::UnknownVariable("customer".to_owned()))
    );
    for variable in Variable::ALL {
        let source = format!("has({}.anything)", variable.as_str());
        assert!(rules().compile(&source).is_ok(), "{source}");
    }
}

#[test]
fn the_length_limit_counts_characters_not_bytes() {
    // `'…' != ''` is eight characters around the text.
    let text = |chars: usize| format!("'{}' != ''", "đ".repeat(chars - 8));
    assert!(rules().compile(&text(SOURCE_CHARS_MAX)).is_ok());
    assert_eq!(
        rules().compile(&text(SOURCE_CHARS_MAX + 1)).err(),
        Some(RuleError::TooLong {
            chars: SOURCE_CHARS_MAX + 1
        })
    );
}

#[test]
fn the_node_limit_holds() {
    // `[1, …, 1] != []`: n ones make n + 3 nodes, three levels deep.
    let list = |nodes: usize| format!("[{}] != []", vec!["1"; nodes - 3].join(", "));
    assert!(rules().compile(&list(NODES_MAX)).is_ok());
    assert_eq!(
        rules().compile(&list(NODES_MAX + 1)).err(),
        Some(RuleError::TooManyNodes)
    );
}

/// Runs `work` on a thread with room for an unoptimised parser: the default 2 MiB test thread
/// overflows at about 16 nested brackets in a debug build (see `dialect::bracket_depth`).
fn on_a_big_stack(work: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(work)
        .expect("spawn")
        .join()
        .expect("the work did not panic");
}

#[test]
fn the_depth_limit_holds() {
    on_a_big_stack(|| {
        // `[[…[1]…]] != []`: the literal sits two levels below the lists around it.
        let nested = |depth: usize| {
            let lists = depth - 2;
            format!("{}1{} != []", "[".repeat(lists), "]".repeat(lists))
        };
        assert!(rules().compile(&nested(DEPTH_MAX)).is_ok());
        assert_eq!(
            rules().compile(&nested(DEPTH_MAX + 1)).err(),
            Some(RuleError::TooDeep),
            "refused by the walk"
        );
    });
}

/// Refused from the text, before the parser recurses: on the default test stack, which an
/// unoptimised parser would overflow and abort on.
#[test]
fn deep_brackets_are_refused_before_parsing() {
    let parens = format!(
        "{}1{} == 1",
        "(".repeat(DEPTH_MAX + 1),
        ")".repeat(DEPTH_MAX + 1)
    );
    assert_eq!(rules().compile(&parens).err(), Some(RuleError::TooDeep));
    let hundred = format!("{}1{} != []", "[".repeat(100), "]".repeat(100));
    assert_eq!(rules().compile(&hundred).err(), Some(RuleError::TooDeep));
}

#[test]
fn brackets_inside_a_string_are_text() {
    let brackets = "(".repeat(DEPTH_MAX + 1);
    for source in [
        format!("order.table != '{brackets}'"),
        format!("order.table != \"{brackets}\""),
        format!("order.table != '''it's {brackets}'''"),
        format!(r"order.table != 'a\'{brackets}'"),
        format!(r"order.table != r'\' && order.table != '{brackets}'"),
    ] {
        assert_eq!(super::dialect::bracket_depth(&source), 0, "{source}");
        assert!(rules().compile(&source).is_ok(), "{source}");
    }
}

/// Safe failures: an error, never a wrap, a panic or a guess.
#[test]
fn what_cannot_be_answered_is_an_error() {
    assert!(matches!(
        answer("order.member_id == 'x'"),
        Err(RuleError::Evaluation(_))
    ));
    assert!(matches!(
        answer("order.total_amount / 0 > 0"),
        Err(RuleError::Evaluation(_))
    ));
    assert!(matches!(
        answer("9223372036854775807 + order.guest_count > 0"),
        Err(RuleError::Evaluation(_))
    ));
    assert!(matches!(
        answer("order.category_ids[9] == 'x'"),
        Err(RuleError::Evaluation(_))
    ));
    assert!(matches!(
        answer("bill.total_amount > 0"),
        Err(RuleError::Evaluation(_))
    ));
    assert_eq!(answer("order.guest_count"), Err(RuleError::NotABoolean));
    assert!(matches!(
        rules().compile("order.guest_count >"),
        Err(RuleError::Syntax(_))
    ));
}

#[test]
fn a_struct_is_refused() {
    assert!(matches!(
        rules().compile("google.protobuf.Int64Value{value: 1} != null"),
        Err(RuleError::StructNotAllowed(_))
    ));
}

#[test]
fn one_compiled_condition_answers_many_times() {
    let condition = rules().compile("order.guest_count > 4").expect("compiles");
    let few = Facts::new().with(
        Variable::Order,
        Fact::Map(BTreeMap::from([("guest_count".to_owned(), Fact::Int(2))])),
    );
    assert_eq!(condition.evaluate(&order()), Ok(true));
    assert_eq!(condition.evaluate(&few), Ok(false));
}

#[test]
fn a_condition_says_which_views_it_reads() {
    let condition = rules()
        .compile("has(order.guest_count) && channel == 'SALES_CHANNEL_DINE_IN'")
        .expect("compiles");
    assert!(condition.reads(Variable::Order));
    assert!(condition.reads(Variable::Channel));
    assert!(!condition.reads(Variable::Device));
}
