// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The flow engine: the checks a package's flows pass at publish and on apply, which steps the
//! till may offer next, and the edge's re-check of a command against its guards
//! ([ADR-0192](../../../docs/adr/0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md)).
//!
//! The flow types are `pos_proto::flow`'s. Conditions are CEL, which this crate does not evaluate
//! (ADR-0193 keeps `pos-rules` out of `pos-core`): every function that needs one takes a
//! [`FlowConditions`], which the cloud and the edge implement over `pos-rules` and tests fake.
//!
//! # What a step being done means
//!
//! A step is **satisfied** when it is done, when it does not apply (its `applies_when` does not
//! hold), or, for an optional step, when the operator skipped it. A step can run when every one of
//! its prerequisites is satisfied ([`missing_prerequisite`]).
//!
//! The edge enforces only **edge guards**: required steps with a `done_when`, a condition over the
//! edge's own facts. Before a command for an action, [`unmet_guard`] finds a step running that
//! action whose guarded prerequisite applies and is not done. A condition that cannot be evaluated
//! counts against the command: a guard fails closed.

use std::collections::{BTreeMap, BTreeSet};

use pos_proto::actions::{Action, ActionEffect};
use pos_proto::flow::{FLOWS_MAX, Flow, FlowStep, ID_CHARS_MAX, STEPS_MAX, StepKind};
use pos_proto::package::Variant;

use crate::package::LABEL_KEY_CHARS_MAX;

/// Where conditions are compiled and evaluated: `pos-rules` on the cloud and the edge, a fake in
/// tests.
pub trait FlowConditions {
    /// Whether `condition` compiles in the dialect. With `edge_facts`, it must also read only the
    /// facts the edge holds, because it is a step's `done_when`. `Err` says why not.
    ///
    /// # Errors
    ///
    /// The reason the condition is refused.
    fn check(&self, condition: &str, edge_facts: bool) -> Result<(), String>;

    /// Whether `condition` holds now. `Err` when it cannot be answered.
    ///
    /// # Errors
    ///
    /// The reason the condition could not be answered.
    fn holds(&self, condition: &str) -> Result<bool, String>;
}

/// Why a package's flows cannot be published or applied (ADR-0192 decision 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlowRefusal {
    /// More than [`FLOWS_MAX`] flows.
    TooManyFlows(usize),
    /// A flow with no step, or more than [`STEPS_MAX`].
    StepCount {
        /// The flow.
        flow_id: String,
        /// How many steps it has.
        steps: usize,
    },
    /// An id that is empty, too long, or not `a`–`z`, `0`–`9` and `_`.
    BadId(String),
    /// A step id used twice in one flow.
    DuplicateStep {
        /// The flow.
        flow_id: String,
        /// The step.
        step_id: String,
    },
    /// A step that names neither a screen nor an action, or both.
    NotOneThing {
        /// The flow.
        flow_id: String,
        /// The step.
        step_id: String,
    },
    /// A required step whose act needs the cloud (`ACTION_EFFECT_EXTERNAL`) and that says nothing
    /// about what happens offline (ADR-0214 decision 5).
    NoOfflineBranch {
        /// The flow.
        flow_id: String,
        /// The step.
        step_id: String,
    },
    /// An offline branch that names neither or both of a note and a step, a malformed note key,
    /// or a step that is not another step of the same flow.
    BadOfflineBranch {
        /// The flow.
        flow_id: String,
        /// The step.
        step_id: String,
    },
    /// A step whose kind is absent or from a newer release.
    UnknownKind {
        /// The flow.
        flow_id: String,
        /// The step.
        step_id: String,
    },
    /// An action the catalogue does not hold.
    UnknownAction(String),
    /// A screen the package does not declare.
    UnknownScreen(String),
    /// A prerequisite that is not a step of the same flow, or is the step itself.
    UnknownPrerequisite {
        /// The flow.
        flow_id: String,
        /// The step that waits.
        step_id: String,
        /// What it waits for.
        prerequisite: String,
    },
    /// Prerequisites that wait on each other, so some step could never run.
    Cycle {
        /// The flow.
        flow_id: String,
        /// The steps in or behind the cycle.
        step_ids: Vec<String>,
    },
    /// A condition that does not compile, or a `done_when` over facts the edge does not hold.
    Condition {
        /// The flow.
        flow_id: String,
        /// The step.
        step_id: String,
        /// Why.
        reason: String,
    },
}

impl core::fmt::Display for FlowRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooManyFlows(count) => write!(f, "{count} flows, over {FLOWS_MAX}"),
            Self::StepCount { flow_id, steps } => {
                write!(f, "flow {flow_id} has {steps} steps; 1 to {STEPS_MAX}")
            }
            Self::BadId(id) => write!(f, "`{id}` is not a valid id"),
            Self::DuplicateStep { flow_id, step_id } => {
                write!(f, "flow {flow_id} declares step {step_id} twice")
            }
            Self::NotOneThing { flow_id, step_id } => write!(
                f,
                "step {step_id} of flow {flow_id} must show one screen or run one action"
            ),
            Self::NoOfflineBranch { flow_id, step_id } => write!(
                f,
                "step {step_id} of flow {flow_id} is required and needs the cloud, so it must say \
                 what happens offline"
            ),
            Self::BadOfflineBranch { flow_id, step_id } => write!(
                f,
                "step {step_id} of flow {flow_id}: an offline branch names one note key or one \
                 other step of the flow"
            ),
            Self::UnknownKind { flow_id, step_id } => {
                write!(
                    f,
                    "step {step_id} of flow {flow_id} has no kind this release knows"
                )
            }
            Self::UnknownAction(id) => write!(f, "`{id}` is not in the action catalogue"),
            Self::UnknownScreen(id) => write!(f, "`{id}` is not a screen of the package"),
            Self::UnknownPrerequisite {
                flow_id,
                step_id,
                prerequisite,
            } => write!(
                f,
                "step {step_id} of flow {flow_id} waits for {prerequisite}, which is not another \
                 step of the flow"
            ),
            Self::Cycle { flow_id, step_ids } => write!(
                f,
                "in flow {flow_id}, steps {} wait on each other",
                step_ids.join(", ")
            ),
            Self::Condition {
                flow_id,
                step_id,
                reason,
            } => write!(f, "step {step_id} of flow {flow_id}: {reason}"),
        }
    }
}

/// Checks a package's flows, every variant of each, before publish and again on apply.
/// `screen_exists` says whether the package declares a screen.
///
/// # Errors
///
/// The first [`FlowRefusal`] found.
pub fn check_flows(
    flows: &[Variant<Flow>],
    screen_exists: &dyn Fn(&str) -> bool,
    conditions: &dyn FlowConditions,
) -> Result<(), FlowRefusal> {
    let ids: BTreeSet<&str> = flows
        .iter()
        .map(|variant| variant.body.flow_id.as_str())
        .collect();
    if ids.len() > FLOWS_MAX {
        return Err(FlowRefusal::TooManyFlows(ids.len()));
    }
    flows
        .iter()
        .try_for_each(|variant| check_flow(&variant.body, screen_exists, conditions))
}

fn check_flow(
    flow: &Flow,
    screen_exists: &dyn Fn(&str) -> bool,
    conditions: &dyn FlowConditions,
) -> Result<(), FlowRefusal> {
    let flow_id = flow.flow_id.as_str();
    check_id(flow_id)?;
    known_action(&flow.start_action_id)?;
    if flow.steps.is_empty() || flow.steps.len() > STEPS_MAX {
        return Err(FlowRefusal::StepCount {
            flow_id: flow_id.to_owned(),
            steps: flow.steps.len(),
        });
    }
    let mut seen = BTreeSet::new();
    for step in &flow.steps {
        check_id(&step.step_id)?;
        if !seen.insert(step.step_id.as_str()) {
            return Err(refusal(flow, step, Refused::Duplicate));
        }
    }
    for step in &flow.steps {
        check_step(flow, step, &seen, screen_exists, conditions)?;
    }
    check_acyclic(flow)
}

/// The step-level refusals that carry only the flow and the step.
#[derive(Clone, Copy)]
enum Refused {
    Duplicate,
    NotOneThing,
    UnknownKind,
    NoOfflineBranch,
    BadOfflineBranch,
}

fn refusal(flow: &Flow, step: &FlowStep, refused: Refused) -> FlowRefusal {
    let flow_id = flow.flow_id.clone();
    let step_id = step.step_id.clone();
    match refused {
        Refused::Duplicate => FlowRefusal::DuplicateStep { flow_id, step_id },
        Refused::NotOneThing => FlowRefusal::NotOneThing { flow_id, step_id },
        Refused::UnknownKind => FlowRefusal::UnknownKind { flow_id, step_id },
        Refused::NoOfflineBranch => FlowRefusal::NoOfflineBranch { flow_id, step_id },
        Refused::BadOfflineBranch => FlowRefusal::BadOfflineBranch { flow_id, step_id },
    }
}

fn check_step(
    flow: &Flow,
    step: &FlowStep,
    step_ids: &BTreeSet<&str>,
    screen_exists: &dyn Fn(&str) -> bool,
    conditions: &dyn FlowConditions,
) -> Result<(), FlowRefusal> {
    match (&step.screen_id, &step.action_id) {
        (Some(screen_id), None) => {
            if !screen_exists(screen_id) {
                return Err(FlowRefusal::UnknownScreen(screen_id.clone()));
            }
        }
        (None, Some(action_id)) => known_action(action_id)?,
        (Some(_), Some(_)) | (None, None) => {
            return Err(refusal(flow, step, Refused::NotOneThing));
        }
    }
    if step.kind.known() == StepKind::Unspecified {
        return Err(refusal(flow, step, Refused::UnknownKind));
    }
    check_offline(flow, step, step_ids)?;
    for prerequisite in &step.prerequisites {
        if *prerequisite == step.step_id || !step_ids.contains(prerequisite.as_str()) {
            return Err(FlowRefusal::UnknownPrerequisite {
                flow_id: flow.flow_id.clone(),
                step_id: step.step_id.clone(),
                prerequisite: prerequisite.clone(),
            });
        }
    }
    let compiled = |condition: &Option<String>, edge_facts: bool| match condition {
        Some(source) => {
            conditions
                .check(source, edge_facts)
                .map_err(|reason| FlowRefusal::Condition {
                    flow_id: flow.flow_id.clone(),
                    step_id: step.step_id.clone(),
                    reason,
                })
        }
        None => Ok(()),
    };
    // An edge guard's `applies_when` is evaluated on the edge too, so it reads only edge facts.
    compiled(&step.applies_when, step.done_when.is_some())?;
    compiled(&step.done_when, true)
}

/// A step's offline branch (ADR-0214 decision 5): when it has one, exactly one of a well-formed
/// note key and another step of the flow; when it has none, it is not a required step whose act
/// needs the cloud.
fn check_offline(
    flow: &Flow,
    step: &FlowStep,
    step_ids: &BTreeSet<&str>,
) -> Result<(), FlowRefusal> {
    let Some(branch) = &step.offline else {
        let external = step
            .action_id
            .as_deref()
            .and_then(Action::find)
            .is_some_and(|action| action.effect == ActionEffect::External);
        return if external && step.kind.known() == StepKind::Required {
            Err(refusal(flow, step, Refused::NoOfflineBranch))
        } else {
            Ok(())
        };
    };
    let fits = match (&branch.skip_note_key, &branch.step_id) {
        (Some(key), None) => {
            !key.is_empty()
                && key.chars().count() <= LABEL_KEY_CHARS_MAX
                && key
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
        }
        (None, Some(other)) => *other != step.step_id && step_ids.contains(other.as_str()),
        (Some(_), Some(_)) | (None, None) => false,
    };
    if fits {
        Ok(())
    } else {
        Err(refusal(flow, step, Refused::BadOfflineBranch))
    }
}

/// Refuses prerequisites that wait on each other: Kahn's walk takes every step whose prerequisites
/// are all taken, and a step never taken is in, or behind, a cycle.
fn check_acyclic(flow: &Flow) -> Result<(), FlowRefusal> {
    let mut taken: BTreeSet<&str> = BTreeSet::new();
    loop {
        let ready: Vec<&str> = flow
            .steps
            .iter()
            .filter(|step| !taken.contains(step.step_id.as_str()))
            .filter(|step| {
                step.prerequisites
                    .iter()
                    .all(|prerequisite| taken.contains(prerequisite.as_str()))
            })
            .map(|step| step.step_id.as_str())
            .collect();
        if ready.is_empty() {
            break;
        }
        taken.extend(ready);
    }
    let stuck: Vec<String> = flow
        .steps
        .iter()
        .filter(|step| !taken.contains(step.step_id.as_str()))
        .map(|step| step.step_id.clone())
        .collect();
    if stuck.is_empty() {
        Ok(())
    } else {
        Err(FlowRefusal::Cycle {
            flow_id: flow.flow_id.clone(),
            step_ids: stuck,
        })
    }
}

fn check_id(id: &str) -> Result<(), FlowRefusal> {
    let shaped = !id.is_empty()
        && id.chars().count() <= ID_CHARS_MAX
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if shaped {
        Ok(())
    } else {
        Err(FlowRefusal::BadId(id.to_owned()))
    }
}

fn known_action(action_id: &str) -> Result<(), FlowRefusal> {
    if Action::find(action_id).is_some() {
        Ok(())
    } else {
        Err(FlowRefusal::UnknownAction(action_id.to_owned()))
    }
}

/// What the till knows of a flow in progress: the steps done and the optional steps skipped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlowProgress {
    /// Steps done.
    pub done: BTreeSet<String>,
    /// Optional steps the operator skipped.
    pub skipped: BTreeSet<String>,
}

/// The first prerequisite of `step` that is not satisfied, so the step cannot run yet: the step the
/// till names when it disables the action (ADR-0192 decision 3). A condition that cannot be
/// answered leaves its step unsatisfied.
#[must_use]
pub fn missing_prerequisite<'f>(
    flow: &'f Flow,
    step: &FlowStep,
    progress: &FlowProgress,
    conditions: &dyn FlowConditions,
) -> Option<&'f FlowStep> {
    step.prerequisites.iter().find_map(|prerequisite| {
        let waited = flow
            .steps
            .iter()
            .find(|candidate| candidate.step_id == *prerequisite)?;
        let satisfied = progress.done.contains(prerequisite)
            || (waited.kind.known() == StepKind::Optional
                && progress.skipped.contains(prerequisite))
            || !applies(waited, conditions);
        (!satisfied).then_some(waited)
    })
}

/// The steps the till may offer now: not done, not skipped, applying, and with every prerequisite
/// satisfied, in the flow's order.
#[must_use]
pub fn available_steps<'f>(
    flow: &'f Flow,
    progress: &FlowProgress,
    conditions: &dyn FlowConditions,
) -> Vec<&'f FlowStep> {
    flow.steps
        .iter()
        .filter(|step| {
            !progress.done.contains(&step.step_id) && !progress.skipped.contains(&step.step_id)
        })
        .filter(|step| applies(step, conditions))
        .filter(|step| missing_prerequisite(flow, step, progress, conditions).is_none())
        .collect()
}

/// Whether a step applies. A condition that cannot be answered applies it, so a guard fails closed.
fn applies(step: &FlowStep, conditions: &dyn FlowConditions) -> bool {
    step.applies_when
        .as_deref()
        .is_none_or(|condition| conditions.holds(condition).unwrap_or(true))
}

/// The step an edge command for `action_id` waits on and the edge sees undone, if any: the
/// refusal `409 FLOW_PREREQUISITE_UNMET` names it (ADR-0192 decision 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnmetGuard {
    /// The flow.
    pub flow_id: String,
    /// The step that is not done.
    pub step_id: String,
}

/// Re-checks a command for `action_id` against `flows`, the variants that apply to the order:
/// every step running the action waits on its guarded prerequisites, and the first that applies
/// and whose `done_when` does not hold, or cannot be answered, is returned.
#[must_use]
pub fn unmet_guard(
    flows: &[&Flow],
    action_id: &str,
    conditions: &dyn FlowConditions,
) -> Option<UnmetGuard> {
    flows.iter().find_map(|flow| {
        let guarded: BTreeMap<&str, &FlowStep> = flow
            .steps
            .iter()
            .filter(|step| step.is_edge_guard())
            .map(|step| (step.step_id.as_str(), step))
            .collect();
        flow.steps
            .iter()
            .filter(|step| step.action_id.as_deref() == Some(action_id))
            .flat_map(|step| step.prerequisites.iter())
            .filter_map(|prerequisite| guarded.get(prerequisite.as_str()))
            .find(|guard| applies(guard, conditions) && !done(guard, conditions))
            .map(|guard| UnmetGuard {
                flow_id: flow.flow_id.clone(),
                step_id: guard.step_id.clone(),
            })
    })
}

fn done(step: &FlowStep, conditions: &dyn FlowConditions) -> bool {
    step.done_when
        .as_deref()
        .is_some_and(|condition| conditions.holds(condition).unwrap_or(false))
}

#[cfg(test)]
mod tests;
