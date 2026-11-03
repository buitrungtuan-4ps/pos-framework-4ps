// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The walk that holds a compiled condition to the dialect (ADR-0193 decision 3).
//!
//! The `cel` crate parses and interprets; it has no type checker, and without the macros a loop
//! macro still parses as a plain call that fails only when it runs. So every node is checked here,
//! at compile time, before a condition is published or applied.

use cel::IdedExpr;
use cel::common::ast::{EntryExpr, Expr, LiteralValue, operators};

use crate::RuleError;
use crate::facts::Variable;

/// The longest condition, in characters.
pub const SOURCE_CHARS_MAX: usize = 1_024;

/// The most nodes in a condition.
pub const NODES_MAX: usize = 256;

/// The deepest a condition may nest, the root at depth 1.
pub const DEPTH_MAX: usize = 32;

/// The loop macros the dialect refuses by name.
pub const LOOP_MACROS: [&str; 5] = [
    operators::ALL,
    operators::EXISTS,
    operators::EXISTS_ONE,
    operators::MAP,
    operators::FILTER,
];

/// Every function a condition may call: comparison, boolean logic, integer arithmetic, `in`,
/// indexing, and four functions on strings and lists. `has()` is a macro, expanded into a field
/// test before this list is read.
pub const FUNCTIONS: [&str; 21] = [
    operators::EQUALS,
    operators::NOT_EQUALS,
    operators::LESS,
    operators::LESS_EQUALS,
    operators::GREATER,
    operators::GREATER_EQUALS,
    operators::LOGICAL_AND,
    operators::LOGICAL_OR,
    operators::LOGICAL_NOT,
    operators::ADD,
    operators::SUBSTRACT,
    operators::MULTIPLY,
    operators::DIVIDE,
    operators::MODULO,
    operators::NEGATE,
    operators::IN,
    operators::INDEX,
    "size",
    "contains",
    "startsWith",
    "endsWith",
];

/// The one function name outside [`FUNCTIONS`] that carries its own refusal: converting to a
/// fraction.
const DOUBLE: &str = "double";

/// The deepest the brackets of `source` nest, outside string literals, checked before parsing.
///
/// The parser descends once per bracket, and its frames are large: an unoptimised build overflows a
/// 2 MiB thread stack at about 16 nested parentheses, before any walk of the tree could refuse
/// them, and an overflow aborts the process rather than unwinding. Counting the brackets in the
/// text first keeps the parser's recursion within [`DEPTH_MAX`] whatever the source.
pub(crate) fn bracket_depth(source: &str) -> usize {
    let chars: Vec<char> = source.chars().collect();
    let mut depth: usize = 0;
    let mut deepest = 0;
    let mut at = 0;
    while let Some(&c) = chars.get(at) {
        match c {
            '(' | '[' | '{' => {
                depth += 1;
                deepest = deepest.max(depth);
                at += 1;
            }
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                at += 1;
            }
            '\'' | '"' => {
                let raw = at > 0 && matches!(chars.get(at - 1), Some('r' | 'R'));
                at = past_string(&chars, at, c, raw);
            }
            _ => at += 1,
        }
    }
    deepest
}

/// The index just past the string literal opening at `at` with `quote`, triple-quoted or not.
fn past_string(chars: &[char], at: usize, quote: char, raw: bool) -> usize {
    let triple = chars.get(at + 1) == Some(&quote) && chars.get(at + 2) == Some(&quote);
    let width = if triple { 3 } else { 1 };
    let mut at = at + width;
    while let Some(&c) = chars.get(at) {
        if c == '\\' && !raw {
            at += 2;
        } else if c == quote && (0..width).all(|k| chars.get(at + k) == Some(&quote)) {
            return at + width;
        } else {
            at += 1;
        }
    }
    at
}

/// Checks every node of `expr` against the dialect.
pub(crate) fn check(expr: &IdedExpr) -> Result<(), RuleError> {
    let mut nodes = 0;
    walk(expr, 1, &mut nodes)
}

fn walk(node: &IdedExpr, depth: usize, nodes: &mut usize) -> Result<(), RuleError> {
    if depth > DEPTH_MAX {
        return Err(RuleError::TooDeep);
    }
    *nodes += 1;
    if *nodes > NODES_MAX {
        return Err(RuleError::TooManyNodes);
    }
    let next = depth + 1;
    match &node.expr {
        Expr::Call(call) => {
            let name = call.func_name.as_str();
            if LOOP_MACROS.contains(&name) {
                return Err(RuleError::LoopMacro(name.to_owned()));
            }
            if name == DOUBLE {
                return Err(RuleError::DoubleNotAllowed);
            }
            if !FUNCTIONS.contains(&name) {
                return Err(RuleError::FunctionNotAllowed(name.to_owned()));
            }
            if let Some(target) = &call.target {
                walk(target, next, nodes)?;
            }
            call.args.iter().try_for_each(|arg| walk(arg, next, nodes))
        }
        Expr::Comprehension(_) => Err(RuleError::LoopMacro("comprehension".to_owned())),
        Expr::Ident(name) => {
            if Variable::from_name(name).is_some() {
                Ok(())
            } else {
                Err(RuleError::UnknownVariable(name.clone()))
            }
        }
        Expr::List(list) => list
            .elements
            .iter()
            .try_for_each(|element| walk(element, next, nodes)),
        Expr::Literal(literal) => match literal {
            LiteralValue::Double(_) => Err(RuleError::DoubleNotAllowed),
            LiteralValue::Boolean(_)
            | LiteralValue::Bytes(_)
            | LiteralValue::Int(_)
            | LiteralValue::Null
            | LiteralValue::String(_)
            | LiteralValue::UInt(_) => Ok(()),
        },
        Expr::Map(map) => map.entries.iter().try_for_each(|entry| match &entry.expr {
            EntryExpr::MapEntry(pair) => {
                walk(&pair.key, next, nodes)?;
                walk(&pair.value, next, nodes)
            }
            EntryExpr::StructField(field) => walk(&field.value, next, nodes),
        }),
        Expr::Select(select) => walk(&select.operand, next, nodes),
        Expr::Struct(structure) => Err(RuleError::StructNotAllowed(structure.type_name.clone())),
        Expr::Unspecified => Err(RuleError::Syntax("an empty expression".to_owned())),
    }
}
