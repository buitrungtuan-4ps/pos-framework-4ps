// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use std::collections::BTreeMap;

use pos_proto::flow::{FLOWS_MAX, Flow, FlowStep, OfflineBranch, STEPS_MAX, StepKind};
use pos_proto::package::{Variant, VariantSelector};
use pos_proto::wire_enum::Open;

use super::{
    FlowConditions, FlowProgress, FlowRefusal, UnmetGuard, available_steps, check_flows,
    missing_prerequisite, unmet_guard,
};

/// A condition table: each condition's answer, or `None` for one that cannot be answered. A
/// condition naming `till.` reads facts the edge does not hold; `bad` does not compile.
#[derive(Default)]
struct Fake(BTreeMap<&'static str, Option<bool>>);

impl Fake {
    fn with(mut self, condition: &'static str, answer: Option<bool>) -> Self {
        self.0.insert(condition, answer);
        self
    }
}

impl FlowConditions for Fake {
    fn check(&self, condition: &str, edge_facts: bool) -> Result<(), String> {
        if condition == "bad" {
            return Err("does not compile".to_owned());
        }
        if edge_facts && condition.contains("till.") {
            return Err("reads what the edge does not hold".to_owned());
        }
        Ok(())
    }

    fn holds(&self, condition: &str) -> Result<bool, String> {
        match self.0.get(condition) {
            Some(Some(answer)) => Ok(*answer),
            Some(None) | None => Err("cannot answer".to_owned()),
        }
    }
}

const SEATED_DINE_IN: &str = "channel == 'SALES_CHANNEL_DINE_IN'";
const GUESTS_GIVEN: &str = "has(order.guest_count) && order.guest_count > 0";

fn step(step_id: &str, action_id: &str, kind: StepKind, prerequisites: &[&str]) -> FlowStep {
    FlowStep {
        step_id: step_id.to_owned(),
        screen_id: None,
        action_id: Some(action_id.to_owned()),
        kind: Open::from_known(kind),
        applies_when: None,
        prerequisites: prerequisites.iter().map(|p| (*p).to_owned()).collect(),
        done_when: None,
        offline: None,
    }
}

/// Roadmap-v3 B8.4 as a flow: dine-in asks the guest count before the first line, and the edge
/// sees it from the order's own guest count.
fn guest_count_first() -> Flow {
    let mut guests = step("guests", "sales.table.seat", StepKind::Required, &[]);
    guests.applies_when = Some(SEATED_DINE_IN.to_owned());
    guests.done_when = Some(GUESTS_GIVEN.to_owned());
    let mut note = step("note", "ui.panel.open", StepKind::Optional, &["guests"]);
    note.action_id = None;
    note.screen_id = Some("kitchen_note".to_owned());
    Flow {
        flow_id: "dine_in".to_owned(),
        start_action_id: "sales.table.seat".to_owned(),
        steps: vec![
            guests,
            note,
            step(
                "first_line",
                "sales.line.add",
                StepKind::Required,
                &["guests", "note"],
            ),
        ],
    }
}

fn at(flow: &mut Flow, index: usize) -> &mut FlowStep {
    flow.steps.get_mut(index).expect("a step")
}

fn base(flow: Flow) -> Variant<Flow> {
    Variant {
        selector: VariantSelector::default(),
        body: flow,
    }
}

fn check(flow: Flow) -> Result<(), FlowRefusal> {
    check_flows(
        &[base(flow)],
        &|screen| screen == "kitchen_note",
        &Fake::default(),
    )
}

#[test]
fn a_well_formed_flow_passes() {
    assert_eq!(check(guest_count_first()), Ok(()));
}

#[test]
fn every_reference_resolves() {
    let mut flow = guest_count_first();
    flow.start_action_id = "sales.table.teleport".to_owned();
    assert_eq!(
        check(flow),
        Err(FlowRefusal::UnknownAction(
            "sales.table.teleport".to_owned()
        ))
    );

    let mut flow = guest_count_first();
    at(&mut flow, 1).screen_id = Some("nowhere".to_owned());
    assert_eq!(
        check(flow),
        Err(FlowRefusal::UnknownScreen("nowhere".to_owned()))
    );

    let mut flow = guest_count_first();
    at(&mut flow, 2).prerequisites.push("dessert".to_owned());
    assert!(matches!(
        check(flow),
        Err(FlowRefusal::UnknownPrerequisite { prerequisite, .. }) if prerequisite == "dessert"
    ));

    let mut flow = guest_count_first();
    at(&mut flow, 0).prerequisites.push("guests".to_owned());
    assert!(matches!(
        check(flow),
        Err(FlowRefusal::UnknownPrerequisite { .. })
    ));
}

#[test]
fn a_step_does_exactly_one_thing_of_a_known_kind() {
    let mut flow = guest_count_first();
    at(&mut flow, 1).action_id = Some("ui.panel.open".to_owned());
    assert!(matches!(check(flow), Err(FlowRefusal::NotOneThing { .. })));

    let mut flow = guest_count_first();
    at(&mut flow, 0).kind = Open::parse("STEP_KIND_SOMETIMES");
    assert!(matches!(check(flow), Err(FlowRefusal::UnknownKind { .. })));
}

/// ADR-0214 decision 5: a required step that needs the cloud says what happens offline, and what
/// it says is one note key or one other step of the flow.
#[test]
fn a_required_external_step_names_its_offline_branch() {
    let lookup = || step("points", "integrations.lookup.run", StepKind::Required, &[]);
    let mut flow = guest_count_first();
    flow.steps.push(lookup());
    assert_eq!(
        check(flow.clone()),
        Err(FlowRefusal::NoOfflineBranch {
            flow_id: "dine_in".to_owned(),
            step_id: "points".to_owned(),
        })
    );

    let branch = |note: Option<&str>, to: Option<&str>| OfflineBranch {
        skip_note_key: note.map(str::to_owned),
        step_id: to.map(str::to_owned),
    };
    for good in [
        branch(Some("loyalty.offline_note"), None),
        branch(None, Some("first_line")),
    ] {
        at(&mut flow, 3).offline = Some(good);
        assert_eq!(check(flow.clone()), Ok(()));
    }
    for bad in [
        branch(None, None),
        branch(Some("loyalty.offline_note"), Some("first_line")),
        branch(Some("Loyalty Offline"), None),
        branch(None, Some("points")),
        branch(None, Some("dessert")),
    ] {
        at(&mut flow, 3).offline = Some(bad);
        assert!(matches!(
            check(flow.clone()),
            Err(FlowRefusal::BadOfflineBranch { step_id, .. }) if step_id == "points"
        ));
    }

    // An optional one may go without: the operator skips it.
    let mut flow = guest_count_first();
    flow.steps.push(step(
        "points",
        "integrations.lookup.run",
        StepKind::Optional,
        &[],
    ));
    assert_eq!(check(flow), Ok(()));
}

#[test]
fn ids_are_unique_and_shaped() {
    let mut flow = guest_count_first();
    at(&mut flow, 2).step_id = "guests".to_owned();
    assert!(matches!(
        check(flow),
        Err(FlowRefusal::DuplicateStep { .. })
    ));

    let mut flow = guest_count_first();
    flow.flow_id = "Dine In".to_owned();
    assert_eq!(check(flow), Err(FlowRefusal::BadId("Dine In".to_owned())));
}

#[test]
fn prerequisites_that_wait_on_each_other_are_refused() {
    let mut flow = guest_count_first();
    at(&mut flow, 0).prerequisites.push("first_line".to_owned());
    assert_eq!(
        check(flow),
        Err(FlowRefusal::Cycle {
            flow_id: "dine_in".to_owned(),
            step_ids: vec![
                "guests".to_owned(),
                "note".to_owned(),
                "first_line".to_owned()
            ],
        })
    );
}

#[test]
fn the_limits_hold() {
    let mut flow = guest_count_first();
    flow.steps = (0..=STEPS_MAX)
        .map(|n| step(&format!("s{n}"), "sales.line.add", StepKind::Optional, &[]))
        .collect();
    assert!(matches!(
        check(flow),
        Err(FlowRefusal::StepCount { steps, .. }) if steps == STEPS_MAX + 1
    ));

    let mut flow = guest_count_first();
    flow.steps.clear();
    assert!(matches!(
        check(flow),
        Err(FlowRefusal::StepCount { steps: 0, .. })
    ));

    let many: Vec<Variant<Flow>> = (0..=FLOWS_MAX)
        .map(|n| {
            let mut flow = guest_count_first();
            flow.flow_id = format!("f{n}");
            base(flow)
        })
        .collect();
    assert_eq!(
        check_flows(&many, &|_| true, &Fake::default()),
        Err(FlowRefusal::TooManyFlows(FLOWS_MAX + 1))
    );
    // Variants of one flow count once.
    let variants = vec![base(guest_count_first()); FLOWS_MAX + 1];
    assert_eq!(check_flows(&variants, &|_| true, &Fake::default()), Ok(()));
}

#[test]
fn a_condition_must_compile_and_a_guard_must_read_edge_facts() {
    let mut flow = guest_count_first();
    at(&mut flow, 1).applies_when = Some("bad".to_owned());
    assert!(matches!(check(flow), Err(FlowRefusal::Condition { .. })));

    // A till-only step may read what only the till holds…
    let mut flow = guest_count_first();
    at(&mut flow, 1).applies_when = Some("till.screen == 'x'".to_owned());
    assert_eq!(check(flow), Ok(()));

    // …a guard may not, in its done_when or its applies_when, since the edge evaluates both.
    let mut flow = guest_count_first();
    at(&mut flow, 0).done_when = Some("till.guests_typed".to_owned());
    assert!(matches!(check(flow), Err(FlowRefusal::Condition { .. })));
    let mut flow = guest_count_first();
    at(&mut flow, 0).applies_when = Some("till.screen == 'x'".to_owned());
    assert!(matches!(check(flow), Err(FlowRefusal::Condition { .. })));
}

#[test]
fn the_till_offers_what_is_ready_and_names_what_is_missing() {
    let flow = guest_count_first();
    let dine_in = Fake::default().with(SEATED_DINE_IN, Some(true));
    let mut progress = FlowProgress::default();

    let ids = |steps: Vec<&FlowStep>| -> Vec<String> {
        steps.into_iter().map(|s| s.step_id.clone()).collect()
    };
    assert_eq!(ids(available_steps(&flow, &progress, &dine_in)), ["guests"]);
    let first_line = flow.steps.get(2).expect("a step");
    assert_eq!(
        missing_prerequisite(&flow, first_line, &progress, &dine_in).map(|s| s.step_id.as_str()),
        Some("guests")
    );

    progress.done.insert("guests".to_owned());
    assert_eq!(ids(available_steps(&flow, &progress, &dine_in)), ["note"]);
    assert_eq!(
        missing_prerequisite(&flow, first_line, &progress, &dine_in).map(|s| s.step_id.as_str()),
        Some("note")
    );

    // An optional step is passed by a tap.
    progress.skipped.insert("note".to_owned());
    assert_eq!(
        ids(available_steps(&flow, &progress, &dine_in)),
        ["first_line"]
    );

    // A step that does not apply counts as satisfied: a takeaway never asks the guest count.
    let takeaway = Fake::default().with(SEATED_DINE_IN, Some(false));
    let fresh = FlowProgress::default();
    assert_eq!(ids(available_steps(&flow, &fresh, &takeaway)), ["note"]);
}

#[test]
fn the_edge_refuses_a_line_before_the_guest_count() {
    let flow = guest_count_first();
    let flows = [&flow];
    let unmet = Some(UnmetGuard {
        flow_id: "dine_in".to_owned(),
        step_id: "guests".to_owned(),
    });

    let no_guests = Fake::default()
        .with(SEATED_DINE_IN, Some(true))
        .with(GUESTS_GIVEN, Some(false));
    assert_eq!(unmet_guard(&flows, "sales.line.add", &no_guests), unmet);

    let guests = Fake::default()
        .with(SEATED_DINE_IN, Some(true))
        .with(GUESTS_GIVEN, Some(true));
    assert_eq!(unmet_guard(&flows, "sales.line.add", &guests), None);

    // Not dine-in: the guard does not apply.
    let takeaway = Fake::default()
        .with(SEATED_DINE_IN, Some(false))
        .with(GUESTS_GIVEN, Some(false));
    assert_eq!(unmet_guard(&flows, "sales.line.add", &takeaway), None);

    // An action no step waits with is not guarded, and the optional note is never enforced.
    assert_eq!(unmet_guard(&flows, "sales.order.fire", &no_guests), None);
}

/// A guard fails closed: a condition the edge cannot answer refuses the command rather than
/// letting it through.
#[test]
fn a_guard_that_cannot_be_answered_refuses() {
    let flow = guest_count_first();
    let unanswerable = Fake::default();
    assert_eq!(
        unmet_guard(&[&flow], "sales.line.add", &unanswerable).map(|u| u.step_id),
        Some("guests".to_owned())
    );
}
