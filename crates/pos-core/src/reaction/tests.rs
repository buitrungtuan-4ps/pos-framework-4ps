// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_proto::flow::Flow;
use pos_proto::package::{Variant, VariantSelector};
use pos_proto::reaction::{REACTIONS_PER_TRIGGER_MAX, Reaction};

use super::{ReactionRefusal, check_reactions};
use crate::flow::FlowConditions;

/// `bad` does not compile; every other condition does.
struct Fake;

impl FlowConditions for Fake {
    fn check(&self, condition: &str, _edge_facts: bool) -> Result<(), String> {
        if condition == "bad" {
            Err("does not compile".to_owned())
        } else {
            Ok(())
        }
    }

    fn holds(&self, _condition: &str) -> Result<bool, String> {
        Ok(true)
    }
}

fn reaction(json: &str) -> Variant<Reaction> {
    Variant {
        selector: VariantSelector::default(),
        body: serde_json::from_str(json).expect("a reaction parses"),
    }
}

fn flows() -> Vec<Variant<Flow>> {
    let flow: Flow = serde_json::from_str(
        r#"{"flow_id":"dine_in","start_action_id":"sales.table.seat","steps":[{"step_id":"guests","action_id":"sales.table.seat","kind":"STEP_KIND_OPTIONAL"}]}"#,
    )
    .expect("a flow parses");
    vec![Variant {
        selector: VariantSelector::default(),
        body: flow,
    }]
}

fn check(reactions: &[Variant<Reaction>]) -> Result<(), ReactionRefusal> {
    check_reactions(reactions, &flows(), &|screen| screen == "order", &Fake)
}

const NOTE: &str = r#"{"reaction_id":"note","when":"REACTION_TRIGGER_LINE_ADDED","if":"order.line_count == 1","then":{"action_id":"ui.panel.open","params":{"panel_id":"note"}}}"#;

#[test]
fn a_ui_act_and_an_edge_write_pass() {
    let check_on_bill = r#"{"reaction_id":"check","when":"REACTION_TRIGGER_BILL_OPENED","then":{"action_id":"billing.check.print"}}"#;
    let on_screen = r#"{"reaction_id":"s","when":"REACTION_TRIGGER_SCREEN_ENTERED","screen_id":"order","then":{"action_id":"ui.panel.open","params":{"panel_id":"note"}}}"#;
    let after_guests = r#"{"reaction_id":"g","when":"REACTION_TRIGGER_STEP_COMPLETED","flow_id":"dine_in","step_id":"guests","then":{"action_id":"ui.screen.open","params":{"screen_id":"order"}}}"#;
    let all = [NOTE, check_on_bill, on_screen, after_guests].map(reaction);
    assert_eq!(check(&all), Ok(()));
    assert_eq!(check(&[]), Ok(()));
}

#[test]
fn a_money_or_external_act_is_refused() {
    let settle = r#"{"reaction_id":"pay","when":"REACTION_TRIGGER_BILL_OPENED","then":{"action_id":"cash.drawer.open","params":{"reason_code_id":"01J00000000000000000000R01"}}}"#;
    assert_eq!(
        check(&[reaction(settle)]),
        Err(ReactionRefusal::ActNotAllowed {
            reaction_id: "pay".to_owned(),
            action_id: "cash.drawer.open".to_owned(),
        })
    );
    let unknown = NOTE.replace("ui.panel.open", "ui.panel.explode");
    assert_eq!(
        check(&[reaction(&unknown)]),
        Err(ReactionRefusal::UnknownAction(
            "ui.panel.explode".to_owned()
        ))
    );
}

#[test]
fn parameters_are_the_acts_and_required_ones_are_given() {
    let missing = NOTE.replace(r#","params":{"panel_id":"note"}"#, "");
    assert!(matches!(
        check(&[reaction(&missing)]),
        Err(ReactionRefusal::Param { param, .. }) if param == "panel_id"
    ));
    let foreign = NOTE.replace(
        r#""panel_id":"note""#,
        r#""panel_id":"note","amount":"100""#,
    );
    assert!(matches!(
        check(&[reaction(&foreign)]),
        Err(ReactionRefusal::Param { param, .. }) if param == "amount"
    ));
}

#[test]
fn a_trigger_names_exactly_what_it_needs() {
    let unnamed = r#"{"reaction_id":"s","when":"REACTION_TRIGGER_SCREEN_ENTERED","then":{"action_id":"ui.panel.open","params":{"panel_id":"note"}}}"#;
    assert_eq!(
        check(&[reaction(unnamed)]),
        Err(ReactionRefusal::Qualifier("s".to_owned()))
    );
    let extra = NOTE.replace(r#""when""#, r#""screen_id":"order","when""#);
    assert_eq!(
        check(&[reaction(&extra)]),
        Err(ReactionRefusal::Qualifier("note".to_owned()))
    );
    let elsewhere = unnamed.replace(r#""then""#, r#""screen_id":"kitchen","then""#);
    assert!(matches!(
        check(&[reaction(&elsewhere)]),
        Err(ReactionRefusal::UnknownTarget { .. })
    ));
    let no_step = r#"{"reaction_id":"g","when":"REACTION_TRIGGER_STEP_COMPLETED","flow_id":"dine_in","step_id":"dessert","then":{"action_id":"ui.panel.open","params":{"panel_id":"note"}}}"#;
    assert!(matches!(
        check(&[reaction(no_step)]),
        Err(ReactionRefusal::UnknownTarget { .. })
    ));
    let newer = NOTE.replace(
        "REACTION_TRIGGER_LINE_ADDED",
        "REACTION_TRIGGER_SHIFT_OPENED",
    );
    assert_eq!(
        check(&[reaction(&newer)]),
        Err(ReactionRefusal::UnknownTrigger("note".to_owned()))
    );
}

#[test]
fn ids_and_conditions_are_checked() {
    let bad_id = NOTE.replace(r#""note","when""#, r#""Note!","when""#);
    assert_eq!(
        check(&[reaction(&bad_id)]),
        Err(ReactionRefusal::BadId("Note!".to_owned()))
    );
    let uncompiled = NOTE.replace("order.line_count == 1", "bad");
    assert!(matches!(
        check(&[reaction(&uncompiled)]),
        Err(ReactionRefusal::Condition { .. })
    ));
}

/// One trigger runs at most sixteen reactions; the variants of one reaction count once.
#[test]
fn a_trigger_has_at_most_sixteen_reactions() {
    let numbered =
        |n: usize| reaction(&NOTE.replace(r#""note","when""#, &format!(r#""note_{n}","when""#)));
    let mut full: Vec<_> = (0..REACTIONS_PER_TRIGGER_MAX).map(numbered).collect();
    full.push(numbered(0));
    assert_eq!(check(&full), Ok(()));
    full.push(numbered(REACTIONS_PER_TRIGGER_MAX));
    assert_eq!(
        check(&full),
        Err(ReactionRefusal::TooManyForTrigger {
            trigger: "REACTION_TRIGGER_LINE_ADDED".to_owned(),
            reactions: REACTIONS_PER_TRIGGER_MAX + 1,
        })
    );
}
