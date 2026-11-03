// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A package's conditions, written in a restricted dialect of CEL
//! ([ADR-0193](../../../docs/adr/0193-conditions-and-reactions-are-written-in-cel.md)).
//!
//! One evaluator for three readers: the cloud compiles a condition to validate a package at
//! publish, the edge compiles it again when it applies the package and evaluates it to re-check a
//! flow's guard (ADR-0192 decision 4), and the till runs this crate compiled to `wasm32`, built once
//! at boot and never on a tap.
//!
//! [`Rules`] is the evaluator, built once. [`Rules::compile`] refuses a condition outside the
//! dialect before it can run:
//!
//! - **No loop macros.** `all`, `exists`, `exists_one`, `map` and `filter` are refused by name,
//!   and so is every function not on the allow-list: comparison, boolean logic, integer
//!   arithmetic, `in`, `size`, `contains`, `startsWith`, `endsWith`, indexing, and `has()`.
//! - **Limits.** At most [`SOURCE_CHARS_MAX`] characters, [`NODES_MAX`] nodes and a depth of
//!   [`DEPTH_MAX`].
//! - **Integers only for money.** A `double` literal and `double()` are refused, and a [`Fact`]
//!   cannot hold a fraction, so an amount is always an integer in minor units.
//! - **Only the declared views.** A condition reads [`Variable`]s and nothing else.
//!
//! [`Condition::evaluate`] answers `true` or `false` over [`Facts`], or a [`RuleError`]: a missing
//! field, an overflow or a division by zero is an error, never a guess.
//!
//! Like `pos-core`, this crate has no I/O and uses no `unwrap`, `expect`, `panic!` or `unsafe`, and
//! a panic inside the evaluator is caught at the boundary and reported as
//! [`RuleError::Panicked`] where the build unwinds (a release build aborts instead).

#![forbid(unsafe_code)]

mod dialect;
mod facts;
#[cfg(test)]
mod tests;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use cel::common::ast::Expr;
use cel::parser::Macro;
use cel::{Context, Env, Program, Value};

pub use dialect::{DEPTH_MAX, FUNCTIONS, LOOP_MACROS, NODES_MAX, SOURCE_CHARS_MAX};
pub use facts::{Fact, Facts, Variable};

/// Why a condition was refused, or could not be answered.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RuleError {
    /// Longer than [`SOURCE_CHARS_MAX`] characters.
    #[error("the condition is {chars} characters long, over {SOURCE_CHARS_MAX}")]
    TooLong {
        /// Its length.
        chars: usize,
    },
    /// Over [`NODES_MAX`] nodes.
    #[error("the condition has more than {NODES_MAX} nodes")]
    TooManyNodes,
    /// Nested deeper than [`DEPTH_MAX`].
    #[error("the condition is nested deeper than {DEPTH_MAX}")]
    TooDeep,
    /// Not CEL.
    #[error("the condition does not parse: {0}")]
    Syntax(String),
    /// A loop macro, which the dialect refuses.
    #[error("`{0}` loops over a list, which a condition may not do")]
    LoopMacro(String),
    /// A function not on [`FUNCTIONS`].
    #[error("`{0}` is not a function a condition may call")]
    FunctionNotAllowed(String),
    /// A `double` literal or conversion: money is an integer in minor units.
    #[error("a condition may not use a fraction; amounts are integers in minor units")]
    DoubleNotAllowed,
    /// A struct literal, which builds a value a condition has no use for.
    #[error("a condition may not build a `{0}`")]
    StructNotAllowed(String),
    /// A name that is not one of the [`Variable`]s.
    #[error("`{0}` is not something a condition can read")]
    UnknownVariable(String),
    /// The condition answered something other than `true` or `false`.
    #[error("the condition did not answer true or false")]
    NotABoolean,
    /// The evaluator could not answer: a missing field, an overflow, a division by zero.
    #[error("the condition could not be evaluated: {0}")]
    Evaluation(String),
    /// The evaluator panicked. Reported, never propagated.
    #[error("the evaluator failed")]
    Panicked,
    /// The evaluator could not be built.
    #[error("the evaluator could not be built: {0}")]
    Setup(String),
}

/// The evaluator: the CEL standard functions with no macros but `has()`. Build it once, at boot or
/// when a package is applied, never on a tap.
#[derive(Clone)]
pub struct Rules {
    env: Arc<Env>,
}

impl Rules {
    /// Builds the evaluator.
    ///
    /// # Errors
    ///
    /// [`RuleError::Setup`] if `has()` cannot be declared, or [`RuleError::Panicked`].
    pub fn new() -> Result<Self, RuleError> {
        guarded(|| {
            let mut env = Env::default().with_stdlib();
            env.add_macro(has())
                .map_err(|error| RuleError::Setup(error.to_string()))?;
            Ok(Self { env: Arc::new(env) })
        })
    }

    /// Compiles a condition, refusing it if it leaves the dialect.
    ///
    /// # Errors
    ///
    /// [`RuleError::TooLong`] before parsing; [`RuleError::Syntax`]; then the first of
    /// [`RuleError::TooDeep`], [`RuleError::TooManyNodes`], [`RuleError::LoopMacro`],
    /// [`RuleError::FunctionNotAllowed`], [`RuleError::DoubleNotAllowed`],
    /// [`RuleError::StructNotAllowed`] or [`RuleError::UnknownVariable`] the walk meets; or
    /// [`RuleError::Panicked`].
    pub fn compile(&self, source: &str) -> Result<Condition, RuleError> {
        let chars = source.chars().count();
        if chars > SOURCE_CHARS_MAX {
            return Err(RuleError::TooLong { chars });
        }
        if dialect::bracket_depth(source) > DEPTH_MAX {
            return Err(RuleError::TooDeep);
        }
        guarded(|| {
            let program = self
                .env
                .compile(source)
                .map_err(|error| RuleError::Syntax(error.to_string()))?;
            dialect::check(program.expression())?;
            Ok(Condition {
                program,
                env: Arc::clone(&self.env),
            })
        })
    }
}

/// A compiled condition, ready to evaluate as many times as needed.
pub struct Condition {
    program: Program,
    env: Arc<Env>,
}

impl Condition {
    /// Answers the condition over `facts`.
    ///
    /// # Errors
    ///
    /// [`RuleError::Evaluation`] when the evaluator cannot answer (a field the facts do not have,
    /// an overflow, a division by zero), [`RuleError::NotABoolean`] for an answer that is not
    /// `true` or `false`, or [`RuleError::Panicked`].
    pub fn evaluate(&self, facts: &Facts) -> Result<bool, RuleError> {
        guarded(|| {
            let mut context = Context::with_env(Arc::clone(&self.env));
            for (variable, fact) in facts.iter() {
                context.add_variable_from_value(variable.as_str(), fact.to_value());
            }
            match self.program.execute(&context) {
                Ok(answer) => {
                    if let Value::Bool(answer) = answer {
                        Ok(answer)
                    } else {
                        Err(RuleError::NotABoolean)
                    }
                }
                Err(error) => Err(RuleError::Evaluation(error.to_string())),
            }
        })
    }
}

impl std::fmt::Debug for Rules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rules").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Condition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Condition").finish_non_exhaustive()
    }
}

/// `has(a.b)`: whether a field is present, which a guard needs and which is not a loop.
fn has() -> Macro {
    Macro::global("has", 1, |helper, _target, args| {
        let Some(arg) = args.pop() else {
            return Err(helper.new_error(0, "has() takes one field"));
        };
        if let Expr::Select(mut select) = arg.expr {
            select.test = true;
            Ok(Some(helper.next_expr(Expr::Select(select))))
        } else {
            Err(helper.new_error(
                arg.id,
                "has() takes a field, such as has(order.guest_count)",
            ))
        }
    })
}

/// Runs `work`, turning a panic into [`RuleError::Panicked`] (ADR-0193 decision 1).
fn guarded<T>(work: impl FnOnce() -> Result<T, RuleError>) -> Result<T, RuleError> {
    catch_unwind(AssertUnwindSafe(work)).unwrap_or(Err(RuleError::Panicked))
}
