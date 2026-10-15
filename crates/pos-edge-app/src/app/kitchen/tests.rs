// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The kitchen's unit tests: firing a line, an order or a course, the courses gate, the stock a
//! fired line consumes, routing to a station, and the waiting set the kitchen read keeps.

use std::sync::Arc;

use pos_core::capability::{Capability, CapabilityContext};
use pos_core::inventory::{Recipe, RecipeBook, RecipeLine};
use pos_fakes::FakeStore;
use pos_proto::floor::{KitchenStation, RoutingRule, StationPlan};
use pos_proto::ids::{CourseId, IngredientId, MenuItemId, OrderId, StationId, TableId};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::{Open, OrderLineState};

use crate::app::tests::{a_line, actor, edge, identity};
use crate::app::{Edge, EdgeSession, LineDraft};
use crate::receipt::InMemoryReceipts;

fn course(n: u128) -> CourseId {
    CourseId::new(Ulid::from_u128(n))
}

/// The same line, declaring the course it goes out on.
fn on_course(n: u128) -> LineDraft {
    LineDraft {
        course_id: Some(course(n)),
        ..a_line()
    }
}

#[test]
fn a_line_is_added_to_a_seated_table_then_fired() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let table = TableId::new(Ulid::from_u128(200));
        edge.seat_table(actor(), table, None).await.expect("seats");

        let line = edge.add_line(actor(), table, a_line()).await.expect("adds");
        assert_eq!(line.state, OrderLineState::Added);
        assert_eq!(
            edge.line_state(line.order_line_id),
            Some(OrderLineState::Added)
        );

        let station = StationId::new(Ulid::from_u128(9));
        let fired = edge
            .fire_line(actor(), line.order_line_id, Some(station))
            .await
            .expect("fires");
        assert_eq!(fired.state, OrderLineState::Fired);
        assert_eq!(
            edge.line_state(line.order_line_id),
            Some(OrderLineState::Fired)
        );
    });
}

/// Fire by course: the starters go, the mains stay, and both happen in one transaction.
///
/// The whole point of ADR-0130. `LineCommand::Fire { course }` and its `courses_enabled` gate
/// have been written and tested in the domain since before there was a course to name, and
/// nothing filled the field — this is the caller that does.
#[test]
fn firing_a_course_sends_that_course_and_leaves_the_rest() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let table = TableId::new(Ulid::from_u128(250));
        edge.seat_table(actor(), table, None).await.expect("seats");

        let starter = edge
            .add_line(actor(), table, on_course(900))
            .await
            .expect("adds");
        let main = edge
            .add_line(actor(), table, on_course(901))
            .await
            .expect("adds");
        // An item on no course at all, which a fire-by-course must not sweep up.
        let uncoursed = edge.add_line(actor(), table, a_line()).await.expect("adds");

        let station = StationId::new(Ulid::from_u128(9));
        let fired = edge
            .fire_course(actor(), starter.order_id, course(900), Some(station))
            .await
            .expect("starters away");

        assert_eq!(fired.len(), 1, "only the line on that course is sent");
        assert_eq!(
            edge.line_state(starter.order_line_id),
            Some(OrderLineState::Fired)
        );
        assert_eq!(
            edge.line_state(main.order_line_id),
            Some(OrderLineState::Added),
            "the main is still waiting"
        );
        assert_eq!(
            edge.line_state(uncoursed.order_line_id),
            Some(OrderLineState::Added),
            "and an item on no course is not swept up by a course fire"
        );
    });
}

/// A course with nothing left unsent is an ordinary race, not a refusal — the rule
/// `fire_order` already sets for a whole order, applied to the narrowing of it.
#[test]
fn firing_a_course_twice_is_answered_rather_than_refused() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let table = TableId::new(Ulid::from_u128(251));
        edge.seat_table(actor(), table, None).await.expect("seats");
        let line = edge
            .add_line(actor(), table, on_course(900))
            .await
            .expect("adds");

        let station = StationId::new(Ulid::from_u128(9));
        let first = edge
            .fire_course(actor(), line.order_id, course(900), Some(station))
            .await
            .expect("starters away");
        assert_eq!(first.len(), 1);

        let second = edge
            .fire_course(actor(), line.order_id, course(900), Some(station))
            .await
            .expect("the second device finds the work done");
        assert!(
            second.is_empty(),
            "a course with nothing unsent answers with an empty list"
        );

        let none_such = edge
            .fire_course(actor(), line.order_id, course(999), Some(station))
            .await
            .expect("a course this order never had is the same answer");
        assert!(none_such.is_empty());
    });
}

/// The gate, at last with a caller. A store that fires everything at once has no starters to
/// send, and asking is refused rather than answered "nothing to do" — which on an order whose
/// starters are all still waiting would be a lie.
#[test]
fn a_store_with_courses_disabled_refuses_to_fire_by_course() {
    pos_fakes::executor::run_ready(async {
        let session = EdgeSession {
            capabilities: CapabilityContext::NONE.with(Capability::Tables),
            ..EdgeSession::bootstrap()
        };
        let edge = Edge::new(
            FakeStore::default(),
            identity(),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds");
        let table = TableId::new(Ulid::from_u128(252));
        edge.seat_table(actor(), table, None).await.expect("seats");
        let line = edge
            .add_line(actor(), table, on_course(900))
            .await
            .expect("adds");

        // A station is named explicitly so the capability is the *only* thing that can refuse
        // this: the bootstrap session publishes no routing plan, and without a fallback station
        // the call fails as `UnroutableLine` instead, which would let this pass for the wrong
        // reason.
        let station = StationId::new(Ulid::from_u128(9));
        let refused = edge
            .fire_course(actor(), line.order_id, course(900), Some(station))
            .await;
        assert!(
            matches!(
                refused,
                Err(super::AppError::Domain(
                    pos_core::error::DomainError::CapabilityDisabled {
                        capability: "courses_enabled"
                    }
                ))
            ),
            "asking is refused, not answered empty: got {refused:?}"
        );
        assert_eq!(
            edge.line_state(line.order_line_id),
            Some(OrderLineState::Added),
            "and nothing was sent"
        );
    });
}

/// The refusal comes before the empty-list shortcut. A course the order has no unsent lines on
/// would return `Ok(vec![])` if the gate were only `decide_line`'s, because no line would ever
/// reach it — so a store with courses off would be told "nothing to do" rather than "you cannot
/// do this", and the control would look like it worked.
#[test]
fn the_courses_gate_is_checked_before_there_is_anything_to_fire() {
    pos_fakes::executor::run_ready(async {
        let session = EdgeSession {
            capabilities: CapabilityContext::NONE.with(Capability::Tables),
            ..EdgeSession::bootstrap()
        };
        let edge = Edge::new(
            FakeStore::default(),
            identity(),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds");
        let table = TableId::new(Ulid::from_u128(253));
        edge.seat_table(actor(), table, None).await.expect("seats");
        // On no course, so a course fire matches nothing and the shortcut would fire first.
        let line = edge.add_line(actor(), table, a_line()).await.expect("adds");

        let station = StationId::new(Ulid::from_u128(9));
        let refused = edge
            .fire_course(actor(), line.order_id, course(900), Some(station))
            .await;
        assert!(
            matches!(
                refused,
                Err(super::AppError::Domain(
                    pos_core::error::DomainError::CapabilityDisabled { .. }
                ))
            ),
            "an empty course on a courses-off store is still a refusal: got {refused:?}"
        );
    });
}

/// Fire-everything still sends everything, courses or not. The narrowing must not have narrowed
/// the thing it was extracted from.
#[test]
fn firing_the_order_still_sends_every_line_whatever_course_it_is_on() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let table = TableId::new(Ulid::from_u128(254));
        edge.seat_table(actor(), table, None).await.expect("seats");
        let first = edge
            .add_line(actor(), table, on_course(900))
            .await
            .expect("adds");
        edge.add_line(actor(), table, on_course(901))
            .await
            .expect("adds");
        edge.add_line(actor(), table, a_line()).await.expect("adds");

        let station = StationId::new(Ulid::from_u128(9));
        let fired = edge
            .fire_order(actor(), first.order_id, Some(station))
            .await
            .expect("send the order");
        assert_eq!(
            fired.len(),
            3,
            "two courses and an uncoursed line, all sent"
        );
    });
}

/// Fire-everything is not fire-by-course, and a store with courses off must still be able to
/// send its food to the kitchen.
///
/// `LineCommand::Fire { course }`'s own doc says `by_course` "fires it as part of a course and
/// requires the `courses_enabled` capability" — it is a statement about *this firing*, not about
/// the line's taxonomy. Both edge paths passed the line's `course_id` instead, so on a store
/// with courses disabled a line that merely *belongs* to a course could not be fired at all:
/// `fire_line` refused it, and `fire_order` refused the whole order with it, because one
/// refusal aborts the batch. The food had no way to reach the kitchen short of voiding the line.
///
/// `courses_enabled` defaults on, so this bites the store that deliberately turns it off — a
/// counter or retail profile, which is exactly the multi-industry case the capability exists
/// for. Nothing gates *adding* a line with a course, which is what makes the state reachable.
#[test]
fn a_store_with_courses_disabled_still_fires_a_line_that_belongs_to_one() {
    pos_fakes::executor::run_ready(async {
        let session = EdgeSession {
            capabilities: CapabilityContext::NONE.with(Capability::Tables),
            ..EdgeSession::bootstrap()
        };
        let edge = Edge::new(
            FakeStore::default(),
            identity(),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds");
        let table = TableId::new(Ulid::from_u128(240));
        edge.seat_table(actor(), table, None).await.expect("seats");

        let draft = LineDraft {
            course_id: Some(CourseId::new(Ulid::from_u128(900))),
            ..a_line()
        };
        // Nothing refuses this, which is what makes the state below reachable at all.
        let line = edge.add_line(actor(), table, draft).await.expect("adds");

        let station = StationId::new(Ulid::from_u128(9));
        let fired = edge
            .fire_line(actor(), line.order_line_id, Some(station))
            .await
            .expect("a store with courses off still sends its food to the kitchen");
        assert_eq!(fired.state, OrderLineState::Fired);
    });
}

/// The same refusal on the batch path, where it costs more: one line carrying a course made the
/// **whole order** unfireable, because `fire_order` writes all of it or none of it.
#[test]
fn a_store_with_courses_disabled_still_fires_a_whole_order() {
    pos_fakes::executor::run_ready(async {
        let session = EdgeSession {
            capabilities: CapabilityContext::NONE.with(Capability::Tables),
            ..EdgeSession::bootstrap()
        };
        let edge = Edge::new(
            FakeStore::default(),
            identity(),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds");
        let table = TableId::new(Ulid::from_u128(241));
        edge.seat_table(actor(), table, None).await.expect("seats");

        let plain = edge.add_line(actor(), table, a_line()).await.expect("adds");
        edge.add_line(
            actor(),
            table,
            LineDraft {
                course_id: Some(CourseId::new(Ulid::from_u128(900))),
                ..a_line()
            },
        )
        .await
        .expect("adds");

        let station = StationId::new(Ulid::from_u128(9));
        let fired = edge
            .fire_order(actor(), plain.order_id, Some(station))
            .await
            .expect("one line on a course does not strand the order");
        assert_eq!(fired.len(), 2, "both lines reach the kitchen");
    });
}

/// The regression B1.1 exists for: a fired line consumes the base recipe **and** every
/// modifier's.
///
/// Before the fix `fire_line` passed `modifiers: Vec::new()` to `decide_line`, so a large pizza
/// deducted the base dough and never the "large" modifier's extra. Nothing failed, nothing
/// logged, and the shelf quietly disagreed with the books by exactly the modifiers — on every
/// fired line, forever. Reverting either half of the fix (the `Vec::new()`, or the modifier
/// field on the record) turns the 150 g below back into 100 g.
#[test]
fn a_fired_line_consumes_its_modifier_recipe_as_well_as_the_base() {
    pos_fakes::executor::run_ready(async {
        let base = MenuItemId::new(Ulid::from_u128(500));
        let large = MenuItemId::new(Ulid::from_u128(501));
        let dough = IngredientId::new(Ulid::from_u128(900));

        // The base takes 100 g of dough; the "large" modifier adds 50 g of the same.
        let mut recipes = RecipeBook::new();
        recipes.insert(
            base,
            Recipe::new(vec![RecipeLine {
                ingredient: dough,
                per_unit: Quantity::from_milli(100_000),
            }]),
        );
        recipes.insert(
            large,
            Recipe::new(vec![RecipeLine {
                ingredient: dough,
                per_unit: Quantity::from_milli(50_000),
            }]),
        );
        let mut session = EdgeSession::bootstrap();
        session.recipes = recipes;
        let edge = Edge::new(
            FakeStore::default(),
            identity(),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds");

        let table = TableId::new(Ulid::from_u128(220));
        edge.seat_table(actor(), table, None).await.expect("seats");
        let mut draft = a_line();
        draft.modifier_menu_item_ids = vec![large];
        let line = edge.add_line(actor(), table, draft).await.expect("adds");
        edge.fire_line(
            actor(),
            line.order_line_id,
            Some(StationId::new(Ulid::from_u128(9))),
        )
        .await
        .expect("fires");

        // Negative because nothing has seeded what the store received yet: this is a
        // consumption total, and goods-in is the flagged follow-up that turns it into stock.
        assert_eq!(
            edge.lock_projection().on_hand(dough),
            Quantity::from_milli(-150_000),
            "base 100 g + modifier 50 g must both leave the shelf at fire (§8)"
        );
    });
}

/// And the other direction, so the test above cannot pass by summing something unrelated: a
/// line with no modifiers consumes only its base.
#[test]
fn a_fired_line_with_no_modifiers_consumes_only_its_base() {
    pos_fakes::executor::run_ready(async {
        let base = MenuItemId::new(Ulid::from_u128(500));
        let dough = IngredientId::new(Ulid::from_u128(900));
        let mut recipes = RecipeBook::new();
        recipes.insert(
            base,
            Recipe::new(vec![RecipeLine {
                ingredient: dough,
                per_unit: Quantity::from_milli(100_000),
            }]),
        );
        let mut session = EdgeSession::bootstrap();
        session.recipes = recipes;
        let edge = Edge::new(
            FakeStore::default(),
            identity(),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds");

        let table = TableId::new(Ulid::from_u128(221));
        edge.seat_table(actor(), table, None).await.expect("seats");
        let line = edge.add_line(actor(), table, a_line()).await.expect("adds");
        edge.fire_line(
            actor(),
            line.order_line_id,
            Some(StationId::new(Ulid::from_u128(9))),
        )
        .await
        .expect("fires");

        assert_eq!(
            edge.lock_projection().on_hand(dough),
            Quantity::from_milli(-100_000),
        );
    });
}

#[test]
fn firing_an_already_fired_line_is_refused() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        let table = TableId::new(Ulid::from_u128(202));
        edge.seat_table(actor(), table, None).await.expect("seats");
        let line = edge.add_line(actor(), table, a_line()).await.expect("adds");
        let station = StationId::new(Ulid::from_u128(9));
        edge.fire_line(actor(), line.order_line_id, Some(station))
            .await
            .expect("first fire");

        // A fired line cannot fire again — the domain refuses the transition.
        let refused = edge
            .fire_line(actor(), line.order_line_id, Some(station))
            .await;
        assert!(matches!(refused, Err(super::AppError::Domain(_))));
    });
}

#[test]
fn a_fired_line_routes_to_the_planned_station_over_the_caller() {
    pos_fakes::executor::run_ready(async {
        let edge = edge();
        // Publish a plan that routes the line's item (500) to station 700; the caller will name a
        // different station (999), which the published routing must override (ADR-0072).
        let planned = StationId::new(Ulid::from_u128(700));
        let plan = StationPlan::new()
            .with_station(KitchenStation {
                station_id: planned,
                name: DisplayName::new("Oven"),
                backup_station_id: None,
                late_after_seconds: None,
                ticket_language: Open::default(),
            })
            .with_rule(RoutingRule {
                station_id: planned,
                menu_item_id: Some(MenuItemId::new(Ulid::from_u128(500))),
                course_id: None,
            });
        edge.apply_session(EdgeSession::bootstrap().with_stations(plan));

        let table = TableId::new(Ulid::from_u128(210));
        edge.seat_table(actor(), table, None).await.expect("seats");
        let line = edge.add_line(actor(), table, a_line()).await.expect("adds");
        let mut device = edge.fanout().subscribe();

        let caller = StationId::new(Ulid::from_u128(999));
        edge.fire_line(actor(), line.order_line_id, Some(caller))
            .await
            .expect("fires");

        let frame = device.try_recv().expect("a fire frame reached the device");
        assert!(frame.contains("sales.order_line.fired"));
        assert!(
            frame.contains(&planned.to_string()),
            "the line routes to the plan's station, not the caller's"
        );
        assert!(
            !frame.contains(&caller.to_string()),
            "the caller's station is ignored when the plan resolves one"
        );
    });
}

#[test]
fn firing_with_no_station_plan_or_caller_is_unroutable() {
    pos_fakes::executor::run_ready(async {
        // The bootstrap session has an empty station plan; with no caller station either, a fire
        // has nowhere to route and is refused rather than published with a blank station.
        let edge = edge();
        let table = TableId::new(Ulid::from_u128(211));
        edge.seat_table(actor(), table, None).await.expect("seats");
        let line = edge.add_line(actor(), table, a_line()).await.expect("adds");

        let refused = edge.fire_line(actor(), line.order_line_id, None).await;
        assert!(matches!(refused, Err(super::AppError::UnroutableLine)));
    });
}

/// A store with no kitchen display never bumps, and its waiting set must still stop growing:
/// past [`super::MAX_COOKING`] the orders fired longest ago give way (ADR-0161).
#[test]
fn the_waiting_set_is_bounded_and_drops_the_oldest() {
    let mut projection = super::Projection::default();
    let fired = |ms: i64| Timestamp::from_milliseconds_since_epoch(ms).expect("a valid instant");
    for n in 0..=super::MAX_COOKING {
        let id = u128::try_from(n).expect("a small count");
        let ms = i64::try_from(n).expect("a small count");
        let order_id = OrderId::new(Ulid::from_u128(1 + id));
        projection.cooking.insert(order_id, fired(1_000 + ms));
        projection.bound_cooking();
    }
    assert_eq!(projection.cooking.len(), super::MAX_COOKING);
    assert!(
        !projection
            .cooking
            .contains_key(&OrderId::new(Ulid::from_u128(1))),
        "the order fired first is the one that gave way"
    );
}

/// A paid order's wait ends with the business day its food was fired in: the kitchen read
/// drops one fired on an earlier day (ADR-0161).
#[test]
fn a_paid_order_from_a_closed_day_leaves_the_board() {
    let edge = edge();
    let yesterday = OrderId::new(Ulid::from_u128(0x0DA7));
    edge.lock_projection().cooking.insert(
        yesterday,
        Timestamp::from_milliseconds_since_epoch(1).expect("a valid instant"),
    );

    assert!(edge.kitchen_orders().is_empty());
    assert!(
        edge.lock_projection().cooking.is_empty(),
        "the read ended the day for it, so it does not come back"
    );
}
