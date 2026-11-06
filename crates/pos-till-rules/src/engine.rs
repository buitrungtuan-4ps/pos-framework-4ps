// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The till's side of the boundary, with no `unsafe` in it: a request as JSON in, an answer as
//! JSON out. Every error is an answer, `{"ok":false,"error":…}`; nothing here panics on any input.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use pos_core::flow::{FlowConditions, FlowProgress, available_steps, missing_prerequisite};
use pos_proto::flow::Flow;
use pos_rules::{Condition, Fact, Facts, Rules, Variable};
use serde::Deserialize;
use serde_json::{Value, json};

/// The largest request the till may send, in bytes. A package's flows are well under it: at most
/// 32 flows of 64 steps, each condition at most 1,024 characters.
pub const REQUEST_BYTES_MAX: usize = 1 << 20;

/// What the till asks.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    /// The flows of the package the till runs: every variant of each, in the package's order.
    /// Every condition is compiled now, so a tap only evaluates.
    Load { flows: Vec<Flow> },
    /// The steps of a flow the till may offer now.
    Available {
        flow_id: String,
        /// Which of the flow's variants, in the order loaded: the one the till chose for the order.
        #[serde(default)]
        variant: usize,
        #[serde(default)]
        progress: Progress,
        #[serde(default)]
        facts: BTreeMap<String, Value>,
    },
    /// The first prerequisite of a step that is not satisfied, if any.
    Missing {
        flow_id: String,
        #[serde(default)]
        variant: usize,
        step_id: String,
        #[serde(default)]
        progress: Progress,
        #[serde(default)]
        facts: BTreeMap<String, Value>,
    },
    /// Whether a condition holds over the facts.
    Holds {
        condition: String,
        #[serde(default)]
        facts: BTreeMap<String, Value>,
    },
}

/// A flow in progress: the steps done and the optional steps skipped.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Progress {
    #[serde(default)]
    done: BTreeSet<String>,
    #[serde(default)]
    skipped: BTreeSet<String>,
}

/// The evaluator and the package loaded into it.
#[derive(Debug, Default)]
pub struct Engine {
    rules: Option<Rules>,
    flows: Vec<Flow>,
    compiled: HashMap<String, Condition>,
}

impl Engine {
    /// Answers one request.
    #[must_use]
    pub fn answer(&mut self, request: &[u8]) -> Vec<u8> {
        let answer = match self.answer_value(request) {
            Ok(value) => value,
            Err(error) => json!({ "ok": false, "error": error }),
        };
        // A `Value` always serialises; an empty answer would be read as an error by the till.
        serde_json::to_vec(&answer).unwrap_or_default()
    }

    fn answer_value(&mut self, request: &[u8]) -> Result<Value, String> {
        if request.len() > REQUEST_BYTES_MAX {
            return Err(format!(
                "the request is {} bytes, over {REQUEST_BYTES_MAX}",
                request.len()
            ));
        }
        let request: Request = serde_json::from_slice(request)
            .map_err(|error| format!("the request is not one this module reads: {error}"))?;
        // Built on the first request, which the till makes at boot, and kept.
        if self.rules.is_none() {
            self.rules = Rules::new().ok();
        }
        match request {
            Request::Load { flows } => Ok(self.load(flows)),
            Request::Available {
                flow_id,
                variant,
                progress,
                facts,
            } => {
                let facts = facts_of(&facts)?;
                let flow = self.flow(&flow_id, variant)?;
                let conditions = self.conditions(facts);
                let steps: Vec<&str> =
                    available_steps(flow, &seen(flow, progress, &conditions), &conditions)
                        .into_iter()
                        .map(|step| step.step_id.as_str())
                        .collect();
                Ok(json!({ "ok": true, "steps": steps }))
            }
            Request::Missing {
                flow_id,
                variant,
                step_id,
                progress,
                facts,
            } => {
                let facts = facts_of(&facts)?;
                let flow = self.flow(&flow_id, variant)?;
                let step = flow
                    .steps
                    .iter()
                    .find(|step| step.step_id == step_id)
                    .ok_or_else(|| format!("the flow `{flow_id}` has no step `{step_id}`"))?;
                let conditions = self.conditions(facts);
                let missing = missing_prerequisite(
                    flow,
                    step,
                    &seen(flow, progress, &conditions),
                    &conditions,
                )
                .map(|step| step.step_id.as_str());
                Ok(json!({ "ok": true, "missing": missing }))
            }
            Request::Holds { condition, facts } => {
                let facts = facts_of(&facts)?;
                let holds = self.conditions(facts).holds(&condition)?;
                Ok(json!({ "ok": true, "holds": holds }))
            }
        }
    }

    /// Builds the evaluator once, and compiles every condition of `flows`. A condition that does
    /// not compile is reported and left out: a step it guards then fails closed, as on the edge.
    fn load(&mut self, flows: Vec<Flow>) -> Value {
        let Some(rules) = self.rules.as_ref() else {
            return json!({ "ok": false, "error": "the rule evaluator could not be built" });
        };
        let mut compiled = HashMap::new();
        let mut refused = Vec::new();
        let sources = flows
            .iter()
            .flat_map(|flow| flow.steps.iter())
            .flat_map(|step| {
                [step.applies_when.as_deref(), step.done_when.as_deref()]
                    .into_iter()
                    .flatten()
            });
        for source in sources {
            if compiled.contains_key(source) {
                continue;
            }
            match rules.compile(source) {
                Ok(condition) => {
                    compiled.insert(source.to_owned(), condition);
                }
                Err(error) => {
                    refused.push(json!({ "condition": source, "error": error.to_string() }));
                }
            }
        }
        self.flows = flows;
        self.compiled = compiled;
        json!({ "ok": true, "flows": self.flows.len(), "refused": refused })
    }

    fn flow(&self, flow_id: &str, variant: usize) -> Result<&Flow, String> {
        self.flows
            .iter()
            .filter(|flow| flow.flow_id == flow_id)
            .nth(variant)
            .ok_or_else(|| format!("no variant {variant} of a flow `{flow_id}` is loaded"))
    }

    fn conditions(&self, facts: Facts) -> Answering<'_> {
        Answering {
            engine: self,
            facts,
        }
    }
}

/// What the till knows of a flow: the steps it saw done or skipped, and every step whose
/// `done_when` holds over the facts, which is a step the edge sees as done too. The facts are the
/// edge's own (an order's guest count, its lines), so a step done on another till counts here.
fn seen(flow: &Flow, progress: Progress, conditions: &dyn FlowConditions) -> FlowProgress {
    let mut done = progress.done;
    for step in &flow.steps {
        let holds = step
            .done_when
            .as_deref()
            .is_some_and(|condition| conditions.holds(condition).unwrap_or(false));
        if holds {
            done.insert(step.step_id.clone());
        }
    }
    FlowProgress {
        done,
        skipped: progress.skipped,
    }
}

/// The loaded conditions, answered over one request's facts.
struct Answering<'e> {
    engine: &'e Engine,
    facts: Facts,
}

impl FlowConditions for Answering<'_> {
    fn check(&self, condition: &str, _edge_facts: bool) -> Result<(), String> {
        let rules = self
            .engine
            .rules
            .as_ref()
            .ok_or("the rule evaluator could not be built")?;
        rules
            .compile(condition)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn holds(&self, condition: &str) -> Result<bool, String> {
        if let Some(compiled) = self.engine.compiled.get(condition) {
            return compiled
                .evaluate(&self.facts)
                .map_err(|error| error.to_string());
        }
        // Not one of the package's: compiled now, which a tap should not need.
        let rules = self
            .engine
            .rules
            .as_ref()
            .ok_or("the rule evaluator could not be built")?;
        rules
            .compile(condition)
            .and_then(|compiled| compiled.evaluate(&self.facts))
            .map_err(|error| error.to_string())
    }
}

/// The facts a request names: each key a view a condition reads (ADR-0193 decision 4), each value
/// a boolean, a whole number, a string, a list or an object of those. A fraction is refused: an
/// amount is an integer in minor units.
fn facts_of(raw: &BTreeMap<String, Value>) -> Result<Facts, String> {
    raw.iter().try_fold(Facts::new(), |facts, (name, value)| {
        let variable = Variable::from_name(name)
            .ok_or_else(|| format!("`{name}` is not something a condition can read"))?;
        Ok(facts.with(variable, fact_of(value, 0)?))
    })
}

/// How deep a fact may nest: deeper than any view the till builds.
const FACT_DEPTH_MAX: usize = 8;

fn fact_of(value: &Value, depth: usize) -> Result<Fact, String> {
    if depth > FACT_DEPTH_MAX {
        return Err(format!("a fact nests deeper than {FACT_DEPTH_MAX}"));
    }
    match value {
        Value::Bool(value) => Ok(Fact::Bool(*value)),
        Value::Number(number) => number.as_i64().map(Fact::Int).ok_or_else(|| {
            format!("{number} is not a whole number; amounts are integers in minor units")
        }),
        Value::String(text) => Ok(Fact::Text(text.clone())),
        Value::Array(items) => items
            .iter()
            .map(|item| fact_of(item, depth + 1))
            .collect::<Result<_, _>>()
            .map(Fact::List),
        Value::Object(fields) => fields
            .iter()
            .map(|(name, item)| fact_of(item, depth + 1).map(|fact| (name.clone(), fact)))
            .collect::<Result<_, _>>()
            .map(Fact::Map),
        Value::Null => Err("a fact is never null: leave it out instead".to_owned()),
    }
}
