// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A package's flows: graphs of steps the till runs and the edge re-checks
//! ([ADR-0192](../../../docs/adr/0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md)).
//!
//! A flow is started by an action and is made of steps. Each step shows a screen or runs an
//! action from the catalogue ([`crate::actions`]); is required or optional; may apply only when a
//! condition holds (`applies_when`); and may wait for other steps (`prerequisites`). A step whose
//! completion the edge can see from its own facts says so in `done_when`, a condition over the
//! edge's views; that makes it an **edge guard**: before a command for an action that waits on it,
//! the edge evaluates `done_when` and refuses with `FLOW_PREREQUISITE_UNMET` if it does not hold
//! (decision 4). A step without `done_when` guides the operator and is not enforced.
//!
//! Conditions are CEL source in the dialect of ADR-0193, compiled and checked by `pos-rules`.
//! `pos-core` validates the graph (`pos_core::flow`). No `deny_unknown_fields`, as for every
//! published node.

use serde::{Deserialize, Serialize};

use crate::wire_enum;
use crate::wire_enum::{Open, is_absent};

/// The most flows a package declares (ADR-0192 decision 2), counted by `flow_id`.
pub const FLOWS_MAX: usize = 32;

/// The most steps in one flow (decision 1).
pub const STEPS_MAX: usize = 64;

/// The longest `flow_id` or `step_id`, in characters of `a`–`z`, `0`–`9` and `_`.
pub const ID_CHARS_MAX: usize = 64;

wire_enum! {
    /// Whether a flow must pass through a step.
    StepKind, prefix = "STEP_KIND";
    /// The flow cannot go past it until it is done, unless its `applies_when` does not hold.
    Required = "REQUIRED",
    /// The operator may skip it with a tap.
    Optional = "OPTIONAL",
}

/// One flow of a package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Flow {
    /// The flow's id within its package. Its variants share it.
    pub flow_id: String,
    /// The catalogue action whose tap starts the flow.
    pub start_action_id: String,
    /// The steps, at most [`STEPS_MAX`], in the order the till offers them when several are
    /// available at once.
    pub steps: Vec<FlowStep>,
}

/// One step of a flow. It names exactly one of `screen_id` and `action_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FlowStep {
    /// The step's id within its flow.
    pub step_id: String,
    /// The package screen the step shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub screen_id: Option<String>,
    /// The catalogue action the step runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub action_id: Option<String>,
    /// Required or optional.
    #[serde(default, skip_serializing_if = "is_absent")]
    pub kind: Open<StepKind>,
    /// When the step applies at all. Absent: always. A step that does not apply is passed over,
    /// and counts as done for the steps that wait on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub applies_when: Option<String>,
    /// The steps of the same flow that must be done before this one can run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prerequisites: Vec<String>,
    /// When the edge sees the step as done, from its own facts. Present only on a step the edge
    /// enforces; a condition over state the edge does not hold is refused at publish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub done_when: Option<String>,
    /// What the flow does when the step's act cannot reach the cloud
    /// ([ADR-0214](../../../docs/adr/0214-a-tills-external-lookup-is-one-catalogue-act.md)
    /// decision 5). Absent by default; a required step whose act is `ACTION_EFFECT_EXTERNAL` is
    /// refused at publish without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub offline: Option<OfflineBranch>,
}

/// A step's offline branch: exactly one of skipping it with a note, or going to another step of
/// the same flow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OfflineBranch {
    /// Skip the step, showing the text of this translation key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub skip_note_key: Option<String>,
    /// Go to this step of the same flow instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub step_id: Option<String>,
}

impl FlowStep {
    /// Whether the edge enforces the step: it is required and says when the edge sees it done.
    #[must_use]
    pub fn is_edge_guard(&self) -> bool {
        self.kind.known() == StepKind::Required && self.done_when.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::{Flow, StepKind};

    const GUEST_COUNT_FIRST: &str = r#"{"flow_id":"dine_in","start_action_id":"sales.table.seat","steps":[{"step_id":"guests","action_id":"sales.table.seat","kind":"STEP_KIND_REQUIRED","applies_when":"channel == 'SALES_CHANNEL_DINE_IN'","done_when":"has(order.guest_count) && order.guest_count > 0"},{"step_id":"first_line","action_id":"sales.line.add","kind":"STEP_KIND_REQUIRED","prerequisites":["guests"]}]}"#;

    /// A flow rides the published document, so its bytes are a contract: it round-trips byte for
    /// byte, and an absent field stays absent.
    #[test]
    fn a_flow_round_trips_byte_for_byte() {
        let flow: Flow = serde_json::from_str(GUEST_COUNT_FIRST).expect("parses");
        let guests = flow.steps.first().expect("a step");
        assert_eq!(guests.kind.known(), StepKind::Required);
        assert!(guests.is_edge_guard());
        assert!(!flow.steps.get(1).expect("a second step").is_edge_guard());
        assert_eq!(
            serde_json::to_string(&flow).expect("serialise"),
            GUEST_COUNT_FIRST
        );
    }

    /// A version carries its flows as variants, and a version without flows is written as before.
    #[test]
    fn a_package_version_carries_its_flows() {
        let without = r#"{"package_id":"01J00000000000000000000P01","package_version_id":"01J00000000000000000000V01","version_label":"March","min_edge_version":"0.15.0"}"#;
        let content: crate::package::PackageContent =
            serde_json::from_str(without).expect("parses");
        assert!(content.flows.is_empty());
        assert_eq!(serde_json::to_string(&content).expect("serialise"), without);

        let head = without.trim_end_matches('}');
        let with = format!(r#"{head},"flows":[{{"body":{GUEST_COUNT_FIRST}}}]}}"#);
        let content: crate::package::PackageContent = serde_json::from_str(&with).expect("parses");
        let flow = &content.flows.first().expect("a flow").body;
        assert_eq!(flow.flow_id, "dine_in");
        assert_eq!(serde_json::to_string(&content).expect("serialise"), with);
    }

    /// An offline branch rides the step and round-trips; a step without one is written as before.
    #[test]
    fn an_offline_branch_round_trips() {
        let with = r#"{"flow_id":"f","start_action_id":"integrations.lookup.run","steps":[{"step_id":"points","action_id":"integrations.lookup.run","kind":"STEP_KIND_REQUIRED","offline":{"skip_note_key":"loyalty.offline"}},{"step_id":"later","action_id":"integrations.lookup.run","kind":"STEP_KIND_OPTIONAL","offline":{"step_id":"points"}}]}"#;
        let flow: Flow = serde_json::from_str(with).expect("parses");
        let skip = flow.steps.first().and_then(|step| step.offline.as_ref());
        assert_eq!(
            skip.and_then(|branch| branch.skip_note_key.as_deref()),
            Some("loyalty.offline")
        );
        assert_eq!(serde_json::to_string(&flow).expect("serialise"), with);
        let without: Flow = serde_json::from_str(GUEST_COUNT_FIRST).expect("parses");
        assert!(without.steps.iter().all(|step| step.offline.is_none()));
    }

    /// A field from a newer release is ignored, and a kind from a newer release is kept as it came.
    #[test]
    fn a_newer_flow_still_reads() {
        let newer = r#"{"flow_id":"f","start_action_id":"sales.table.seat","steps":[{"step_id":"s","screen_id":"guests","kind":"STEP_KIND_SOMETIMES","hint":"x"}]}"#;
        let flow: Flow = serde_json::from_str(newer).expect("parses");
        let step = flow.steps.first().expect("a step");
        assert!(step.kind.is_unrecognised());
        assert!(!step.is_edge_guard());
    }
}
