// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A package's reactions: *when* a trigger fires on the till, *if* a condition holds, *then* run
//! one catalogue act with fixed parameters
//! ([ADR-0193](../../../docs/adr/0193-conditions-and-reactions-are-written-in-cel.md) decision 5).
//!
//! The trigger comes from a fixed list ([`ReactionTrigger`]). A screen entered names its screen and
//! a step completed names its flow and step; the other triggers name nothing more. The condition is
//! CEL source in the dialect of ADR-0193, compiled by `pos-rules`. The act is one of the catalogue
//! ([`crate::actions`]) whose effect is `ACTION_EFFECT_UI` or `ACTION_EFFECT_EDGE_WRITE`: a money or
//! external act is only ever offered to a person, as a flow step. `pos-core` checks all of it
//! (`pos_core::reaction`). No `deny_unknown_fields`, as for every published node.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::wire_enum;
use crate::wire_enum::Open;

/// The most reactions one trigger runs (ADR-0193 decision 5), counted by `reaction_id` among those
/// with the same trigger and the same screen, or flow and step.
pub const REACTIONS_PER_TRIGGER_MAX: usize = 16;

wire_enum! {
    /// What fires a reaction on the till.
    ReactionTrigger, prefix = "REACTION_TRIGGER";
    /// A screen of the package was shown; the reaction names it in `screen_id`.
    ScreenEntered = "SCREEN_ENTERED",
    /// A step of a flow was done; the reaction names it in `flow_id` and `step_id`.
    StepCompleted = "STEP_COMPLETED",
    /// A line was added to an order.
    LineAdded = "LINE_ADDED",
    /// An order was opened, a table seated included.
    OrderOpened = "ORDER_OPENED",
    /// A bill was opened.
    BillOpened = "BILL_OPENED",
}

/// One reaction of a package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Reaction {
    /// The reaction's id within its package. Its variants share it.
    pub reaction_id: String,
    /// What fires it.
    pub when: Open<ReactionTrigger>,
    /// The screen whose showing fires it: present exactly for `REACTION_TRIGGER_SCREEN_ENTERED`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub screen_id: Option<String>,
    /// The flow of the step whose completion fires it: present exactly for
    /// `REACTION_TRIGGER_STEP_COMPLETED`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub flow_id: Option<String>,
    /// The step whose completion fires it, with `flow_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub step_id: Option<String>,
    /// When it runs at all, over the till's views. Absent: whenever it fires.
    #[serde(rename = "if", default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub condition: Option<String>,
    /// The act it runs.
    pub then: ReactionAct,
}

/// The act a reaction runs, with parameters fixed in the package. An id parameter left out is the
/// subject's own on the till (the order, table or bill the trigger fired for), as for a button.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReactionAct {
    /// A catalogue act whose effect is UI or an edge write.
    pub action_id: String,
    /// Its parameters by name, each as text. Every required parameter is here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::{Reaction, ReactionTrigger};

    const NOTE_ON_FIRST_LINE: &str = r#"{"reaction_id":"note_first","when":"REACTION_TRIGGER_LINE_ADDED","if":"order.line_count == 1","then":{"action_id":"ui.panel.open","params":{"panel_id":"note"}}}"#;

    /// A reaction rides the published document, so its bytes are a contract: it round-trips byte
    /// for byte, and an absent field stays absent.
    #[test]
    fn a_reaction_round_trips_byte_for_byte() {
        let reaction: Reaction = serde_json::from_str(NOTE_ON_FIRST_LINE).expect("parses");
        assert_eq!(reaction.when.known(), ReactionTrigger::LineAdded);
        assert_eq!(reaction.condition.as_deref(), Some("order.line_count == 1"));
        assert_eq!(
            serde_json::to_string(&reaction).expect("serialise"),
            NOTE_ON_FIRST_LINE
        );
    }

    /// A version carries its reactions as variants, and a version without any is written as before.
    #[test]
    fn a_package_version_carries_its_reactions() {
        let without = r#"{"package_id":"01J00000000000000000000P01","package_version_id":"01J00000000000000000000V01","version_label":"March","min_edge_version":"0.15.0"}"#;
        let content: crate::package::PackageContent =
            serde_json::from_str(without).expect("parses");
        assert!(content.reactions.is_empty());
        assert_eq!(serde_json::to_string(&content).expect("serialise"), without);

        let head = without.trim_end_matches('}');
        let with = format!(r#"{head},"reactions":[{{"body":{NOTE_ON_FIRST_LINE}}}]}}"#);
        let content: crate::package::PackageContent = serde_json::from_str(&with).expect("parses");
        let reaction = &content.reactions.first().expect("a reaction").body;
        assert_eq!(reaction.reaction_id, "note_first");
        assert_eq!(serde_json::to_string(&content).expect("serialise"), with);
    }

    /// A field or a trigger from a newer release still reads, the trigger kept as it came.
    #[test]
    fn a_newer_reaction_still_reads() {
        let newer = r#"{"reaction_id":"r","when":"REACTION_TRIGGER_SHIFT_OPENED","then":{"action_id":"ui.panel.open"},"hint":"x"}"#;
        let reaction: Reaction = serde_json::from_str(newer).expect("parses");
        assert!(reaction.when.is_unrecognised());
        assert!(reaction.then.params.is_empty());
    }
}
