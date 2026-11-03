// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The action catalogue against the route table
//! ([ADR-0191](../../../../../docs/adr/0191-every-act-is-in-a-typed-catalogue.md), "a test per
//! entry").

use std::collections::BTreeSet;

use pos_proto::actions::{ActionEffect, CATALOGUE};

use super::{ROUTE_PERMISSIONS, RouteGate};

/// The edge's money commands: the routes whose command settles, voids, reduces or regroups a bill,
/// or moves cash into, out of or through a drawer (ADR-0191 decision 2). An act calling one is a
/// money act, and a money act calls only these.
const MONEY_ROUTES: &[&str] = &[
    "POST /api/bills/{id}/discount",
    "POST /api/bills/{id}/fees/{fee_id}/waive",
    "POST /api/bills/{id}/merge",
    "POST /api/bills/{id}/settle",
    "POST /api/bills/{id}/split",
    "POST /api/bills/{id}/void",
    "POST /api/bills/{id}/void_settled",
    "POST /api/drawer/open",
    "POST /api/shifts",
    "POST /api/shifts/{id}/close",
    "POST /api/shifts/{id}/count",
    "POST /api/shifts/{id}/paid-in",
    "POST /api/shifts/{id}/paid-out",
    "POST /api/shifts/{id}/safe-drop",
    "POST /api/shifts:batch_close",
];

fn gate(route: &str) -> Option<RouteGate> {
    ROUTE_PERMISSIONS
        .iter()
        .find(|(tabled, _gate)| *tabled == route)
        .map(|(_route, gate)| *gate)
}

/// The refusal tokens the edge answers with: the string arms of `error_reason`.
fn refusal_tokens() -> BTreeSet<&'static str> {
    let source = include_str!("../mod.rs");
    let start = source
        .find("pub(crate) fn error_reason")
        .expect("error_reason is in http/mod.rs");
    let body = &source[start..];
    let body = &body[..body.find("\n}\n").expect("error_reason ends")];
    body.split("=> \"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .collect()
}

#[test]
fn an_act_needs_exactly_what_its_routes_need() {
    for action in CATALOGUE {
        let id = action.action_id;
        let needs: BTreeSet<&str> = action.permissions.iter().copied().collect();
        for route in action.routes {
            let Some(RouteGate::Person(permissions)) = gate(route) else {
                panic!("{id} calls {route}, which is not a route a person acts through");
            };
            let gated: BTreeSet<&str> = permissions.iter().map(|p| p.meta().id).collect();
            assert_eq!(needs, gated, "{id} names what {route} needs");
        }
    }
}

#[test]
fn every_route_a_person_acts_through_is_an_act() {
    let called: BTreeSet<&str> = CATALOGUE
        .iter()
        .flat_map(|action| action.routes.iter().copied())
        .collect();
    for (route, gate) in ROUTE_PERMISSIONS {
        if route.starts_with("GET ") || matches!(gate, RouteGate::NoPerson(_)) {
            continue;
        }
        assert!(
            called.contains(route),
            "{route} changes something a person does, so an act in pos_proto::actions calls it"
        );
    }
}

#[test]
fn an_act_is_money_exactly_when_its_route_is() {
    for action in CATALOGUE {
        for route in action.routes {
            assert_eq!(
                action.effect == ActionEffect::Money,
                MONEY_ROUTES.contains(route),
                "{}: {route} {} money, so the act's effect class must say so",
                action.action_id,
                if MONEY_ROUTES.contains(route) {
                    "moves"
                } else {
                    "moves no"
                }
            );
        }
    }
    for route in MONEY_ROUTES {
        assert!(gate(route).is_some(), "{route} is a published route");
    }
}

#[test]
fn every_predictable_refusal_is_one_the_edge_answers_with() {
    let tokens = refusal_tokens();
    assert!(
        tokens.contains("OPEN_SHIFT_REQUIRED"),
        "the scan reads the arms"
    );
    for action in CATALOGUE {
        for refusal in action.refusals {
            assert!(
                tokens.contains(refusal),
                "{}: {refusal} is not a reason the edge answers with",
                action.action_id
            );
        }
    }
}
