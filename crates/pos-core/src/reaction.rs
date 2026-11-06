// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The checks a package's reactions pass on apply, beside its flows
//! ([ADR-0193](../../../docs/adr/0193-conditions-and-reactions-are-written-in-cel.md) decision 5).
//!
//! The reaction types are `pos_proto::reaction`'s. A reaction is refused when its id is not shaped
//! like a flow's, its trigger is unknown or names the wrong things, its trigger already has
//! [`REACTIONS_PER_TRIGGER_MAX`] reactions, its act is not in the catalogue, moves money or calls
//! out, its parameters are not the act's or leave a required one out, or its condition does not
//! compile. Conditions go through [`FlowConditions`], as the flows' do, so `pos-core` does not
//! depend on `pos-rules`. Running a reaction is the till's: at most that many per trigger, under the
//! signed-in person's permissions, and never in answer to another reaction's act.

use std::collections::{BTreeMap, BTreeSet};

use pos_proto::actions::{Action, ActionEffect};
use pos_proto::flow::{Flow, ID_CHARS_MAX};
use pos_proto::package::Variant;
use pos_proto::reaction::{REACTIONS_PER_TRIGGER_MAX, Reaction, ReactionTrigger};

use crate::flow::FlowConditions;

#[cfg(test)]
mod tests;

/// Why a package's reactions cannot be applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReactionRefusal {
    /// An id that is empty, too long, or not `a`–`z`, `0`–`9` and `_`.
    BadId(String),
    /// A trigger that is absent or from a newer release.
    UnknownTrigger(String),
    /// A trigger without what it must name, or naming what it must not.
    Qualifier(String),
    /// A screen, or a flow's step, that the package does not declare.
    UnknownTarget {
        /// The reaction.
        reaction_id: String,
        /// What it names.
        target: String,
    },
    /// More than [`REACTIONS_PER_TRIGGER_MAX`] reactions for one trigger.
    TooManyForTrigger {
        /// The trigger, as its wire token.
        trigger: String,
        /// How many reactions it has.
        reactions: usize,
    },
    /// An act the catalogue does not hold.
    UnknownAction(String),
    /// An act whose effect a reaction may not run: money or external.
    ActNotAllowed {
        /// The reaction.
        reaction_id: String,
        /// The act.
        action_id: String,
    },
    /// A parameter the act does not take, or a required one left out.
    Param {
        /// The reaction.
        reaction_id: String,
        /// The parameter.
        param: String,
    },
    /// A condition that does not compile.
    Condition {
        /// The reaction.
        reaction_id: String,
        /// Why.
        reason: String,
    },
}

impl core::fmt::Display for ReactionRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadId(id) => write!(f, "`{id}` is not a valid id"),
            Self::UnknownTrigger(id) => {
                write!(f, "reaction {id} has no trigger this release knows")
            }
            Self::Qualifier(id) => write!(
                f,
                "reaction {id} must name a screen for a screen entered, a flow and step for a \
                 step completed, and nothing else"
            ),
            Self::UnknownTarget {
                reaction_id,
                target,
            } => write!(
                f,
                "reaction {reaction_id} names {target}, which the package does not declare"
            ),
            Self::TooManyForTrigger { trigger, reactions } => write!(
                f,
                "{trigger} has {reactions} reactions, over {REACTIONS_PER_TRIGGER_MAX}"
            ),
            Self::UnknownAction(id) => write!(f, "`{id}` is not in the action catalogue"),
            Self::ActNotAllowed {
                reaction_id,
                action_id,
            } => write!(
                f,
                "reaction {reaction_id} cannot run {action_id}: only a UI act or an edge write \
                 may be a reaction's then"
            ),
            Self::Param { reaction_id, param } => write!(
                f,
                "reaction {reaction_id}: `{param}` is not a parameter of its act, or is required \
                 and missing"
            ),
            Self::Condition {
                reaction_id,
                reason,
            } => write!(f, "reaction {reaction_id}: {reason}"),
        }
    }
}

/// Checks a package's reactions, every variant of each, on apply. `flows` are the package's, for a
/// step-completed trigger; `screen_exists` says whether the package declares a screen.
///
/// # Errors
///
/// The first [`ReactionRefusal`] found.
pub fn check_reactions(
    reactions: &[Variant<Reaction>],
    flows: &[Variant<Flow>],
    screen_exists: &dyn Fn(&str) -> bool,
    conditions: &dyn FlowConditions,
) -> Result<(), ReactionRefusal> {
    let mut per_trigger: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    for variant in reactions {
        let reaction = &variant.body;
        check_reaction(reaction, flows, screen_exists, conditions)?;
        let key = format!(
            "{}{}{}{}",
            reaction.when.as_wire(),
            qualifier(reaction.screen_id.as_deref()),
            qualifier(reaction.flow_id.as_deref()),
            qualifier(reaction.step_id.as_deref()),
        );
        let ids = per_trigger.entry(key).or_default();
        ids.insert(reaction.reaction_id.as_str());
        if ids.len() > REACTIONS_PER_TRIGGER_MAX {
            return Err(ReactionRefusal::TooManyForTrigger {
                trigger: reaction.when.as_wire().to_owned(),
                reactions: ids.len(),
            });
        }
    }
    Ok(())
}

fn qualifier(part: Option<&str>) -> String {
    part.map_or_else(String::new, |part| format!("/{part}"))
}

fn check_reaction(
    reaction: &Reaction,
    flows: &[Variant<Flow>],
    screen_exists: &dyn Fn(&str) -> bool,
    conditions: &dyn FlowConditions,
) -> Result<(), ReactionRefusal> {
    let id = reaction.reaction_id.as_str();
    check_id(id)?;
    let target = |target: String| ReactionRefusal::UnknownTarget {
        reaction_id: id.to_owned(),
        target,
    };
    let names = (
        reaction.screen_id.as_deref(),
        reaction.flow_id.as_deref(),
        reaction.step_id.as_deref(),
    );
    match (reaction.when.known(), names) {
        (ReactionTrigger::Unspecified, _) => {
            return Err(ReactionRefusal::UnknownTrigger(id.to_owned()));
        }
        (ReactionTrigger::ScreenEntered, (Some(screen), None, None)) => {
            if !screen_exists(screen) {
                return Err(target(format!("screen {screen}")));
            }
        }
        (ReactionTrigger::StepCompleted, (None, Some(flow), Some(step))) => {
            let declared = flows.iter().any(|variant| {
                variant.body.flow_id == flow
                    && variant
                        .body
                        .steps
                        .iter()
                        .any(|candidate| candidate.step_id == step)
            });
            if !declared {
                return Err(target(format!("step {step} of flow {flow}")));
            }
        }
        (
            ReactionTrigger::LineAdded | ReactionTrigger::OrderOpened | ReactionTrigger::BillOpened,
            (None, None, None),
        ) => {}
        _ => return Err(ReactionRefusal::Qualifier(id.to_owned())),
    }
    check_act(reaction)?;
    if let Some(condition) = &reaction.condition {
        conditions
            .check(condition, false)
            .map_err(|reason| ReactionRefusal::Condition {
                reaction_id: id.to_owned(),
                reason,
            })?;
    }
    Ok(())
}

fn check_act(reaction: &Reaction) -> Result<(), ReactionRefusal> {
    let id = reaction.reaction_id.as_str();
    let action_id = reaction.then.action_id.as_str();
    let action = Action::find(action_id)
        .ok_or_else(|| ReactionRefusal::UnknownAction(action_id.to_owned()))?;
    if !matches!(action.effect, ActionEffect::Ui | ActionEffect::EdgeWrite) {
        return Err(ReactionRefusal::ActNotAllowed {
            reaction_id: id.to_owned(),
            action_id: action_id.to_owned(),
        });
    }
    let param = |param: &str| ReactionRefusal::Param {
        reaction_id: id.to_owned(),
        param: param.to_owned(),
    };
    if let Some(unknown) = reaction.then.params.keys().find(|name| {
        !action
            .params
            .iter()
            .any(|declared| declared.name == name.as_str())
    }) {
        return Err(param(unknown));
    }
    match action
        .params
        .iter()
        .find(|declared| declared.required && !reaction.then.params.contains_key(declared.name))
    {
        Some(missing) => Err(param(missing.name)),
        None => Ok(()),
    }
}

fn check_id(id: &str) -> Result<(), ReactionRefusal> {
    let shaped = !id.is_empty()
        && id.chars().count() <= ID_CHARS_MAX
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if shaped {
        Ok(())
    } else {
        Err(ReactionRefusal::BadId(id.to_owned()))
    }
}
