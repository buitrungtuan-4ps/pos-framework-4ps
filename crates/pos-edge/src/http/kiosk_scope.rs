// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A kiosk reaches only what self-order needs, and only on the orders it opened
//! ([ADR-0224](../../../../docs/adr/0224-self-order-is-a-guest-page-and-a-kiosk.md) decision 4).
//!
//! The fixed kiosk set already refuses a kiosk every act on money. This gate, inside the signed-in
//! one, refuses everything else a kiosk's own record has no business with: any route not in
//! [`kiosk_route`]'s list, and any order or line it did not open. A person signed in on the same
//! device passes straight through, so a manager who has left the kiosk works the till as ever.
//!
//! What a kiosk may do: read the menu, the store's language and layout, its package and the reason
//! list; open a tableless order; read, ring, change, void and send to be paid an order it opened;
//! close that order when it is empty, and pay its bill by card through the terminal (ADR-0230). Nothing else, reads included, so it cannot list the
//! store's orders or read another till's.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::ids::{BillId, EmployeeId, OrderId, OrderLineId};

use crate::app::Edge;

/// What the gate asks of the store, so it can sit over routers of any state type.
pub(crate) trait KioskScope: Send + Sync {
    /// Whether `employee_id` is a kiosk's own record.
    fn is_kiosk(&self, employee_id: EmployeeId) -> bool;
    /// Whether `employee_id` opened `order_id`, still open.
    fn opened_by(&self, order_id: OrderId, employee_id: EmployeeId) -> bool;
    /// The order a line is on.
    fn order_of_line(&self, line_id: OrderLineId) -> Option<OrderId>;
    /// The order a bill is for.
    fn order_of_bill(&self, bill_id: BillId) -> Option<OrderId>;
}

impl<S: EventStore + Send + Sync> KioskScope for Edge<S> {
    fn is_kiosk(&self, employee_id: EmployeeId) -> bool {
        Self::is_kiosk(self, employee_id)
    }

    fn opened_by(&self, order_id: OrderId, employee_id: EmployeeId) -> bool {
        Self::opened_by(self, order_id, employee_id)
    }

    fn order_of_line(&self, line_id: OrderLineId) -> Option<OrderId> {
        Self::order_of_line(self, line_id)
    }

    fn order_of_bill(&self, bill_id: BillId) -> Option<OrderId> {
        Self::order_of_bill(self, bill_id)
    }
}

/// What a route a kiosk may call acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// The store's own facts, or a new order: nothing of anybody else's.
    Free,
    /// The order the path names.
    Order,
    /// The line the path names, by its order.
    Line,
    /// The bill the path names, by its order.
    Bill,
}

/// The kiosk's routes, as `docs/snapshots/routes.txt` writes them.
const KIOSK_ROUTES: &[(&str, &str, Reach)] = &[
    ("GET", "/api/menu", Reach::Free),
    ("GET", "/api/locale", Reach::Free),
    ("GET", "/api/layout", Reach::Free),
    ("GET", "/api/package", Reach::Free),
    ("GET", "/api/reason-codes", Reach::Free),
    ("POST", "/api/orders", Reach::Free),
    ("GET", "/api/orders/{id}/check", Reach::Order),
    ("POST", "/api/orders/{id}/lines", Reach::Order),
    ("POST", "/api/orders/{id}/bill", Reach::Order),
    ("POST", "/api/orders/{id}/close", Reach::Order),
    ("POST", "/api/lines/{id}/quantity", Reach::Line),
    ("POST", "/api/lines/{id}/void", Reach::Line),
    // A card at the kiosk, through the terminal, for the whole of its own bill (ADR-0230
    // decision 7): the route admits a kiosk on exactly those terms.
    ("POST", "/api/bills/{id}/card", Reach::Bill),
];

/// The route `method path` is, if it is a kiosk's, and the id its `{id}` names.
fn kiosk_route<'a>(method: &Method, path: &'a str) -> Option<(Reach, Option<&'a str>)> {
    let segments: Vec<&str> = path.split('/').collect();
    KIOSK_ROUTES.iter().find_map(|(verb, pattern, reach)| {
        if method.as_str() != *verb {
            return None;
        }
        let parts: Vec<&str> = pattern.split('/').collect();
        if parts.len() != segments.len() {
            return None;
        }
        let mut id = None;
        for (part, segment) in parts.iter().zip(&segments) {
            if *part == "{id}" {
                id = Some(*segment);
            } else if part != segment {
                return None;
            }
        }
        Some((*reach, id))
    })
}

/// Whether the kiosk `employee_id` may make this request.
fn allowed(scope: &dyn KioskScope, employee_id: EmployeeId, method: &Method, path: &str) -> bool {
    match kiosk_route(method, path) {
        Some((Reach::Free, _)) => true,
        Some((Reach::Order, Some(id))) => id
            .parse::<OrderId>()
            .is_ok_and(|order| scope.opened_by(order, employee_id)),
        Some((Reach::Line, Some(id))) => id
            .parse::<OrderLineId>()
            .ok()
            .and_then(|line| scope.order_of_line(line))
            .is_some_and(|order| scope.opened_by(order, employee_id)),
        Some((Reach::Bill, Some(id))) => id
            .parse::<BillId>()
            .ok()
            .and_then(|bill| scope.order_of_bill(bill))
            .is_some_and(|order| scope.opened_by(order, employee_id)),
        _ => false,
    }
}

/// The gate: a kiosk's own record passes only to [`KIOSK_ROUTES`], on what it opened; anyone
/// else passes. Refused `403 KIOSK_NOT_ALLOWED`.
pub(crate) async fn kiosk_gate(
    State(scope): State<Arc<dyn KioskScope>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(actor) = request.extensions().get::<Actor>().copied() else {
        return next.run(request).await;
    };
    if !scope.is_kiosk(actor.employee_id)
        || allowed(
            scope.as_ref(),
            actor.employee_id,
            request.method(),
            request.uri().path(),
        )
    {
        return next.run(request).await;
    }
    super::refusal(
        StatusCode::FORBIDDEN,
        "KIOSK_NOT_ALLOWED",
        "a kiosk may not do this".to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::{KIOSK_ROUTES, Reach, kiosk_route};
    use axum::http::Method;

    #[test]
    fn a_kiosk_route_is_matched_by_its_verb_and_every_fixed_segment() {
        assert_eq!(
            kiosk_route(&Method::POST, "/api/orders/01J0/lines"),
            Some((Reach::Order, Some("01J0")))
        );
        assert_eq!(kiosk_route(&Method::GET, "/api/orders/01J0/lines"), None);
        assert_eq!(kiosk_route(&Method::GET, "/api/orders/open"), None);
        assert_eq!(kiosk_route(&Method::POST, "/api/bills/01J0/settle"), None);
        assert_eq!(
            kiosk_route(&Method::POST, "/api/orders"),
            Some((Reach::Free, None))
        );
    }

    #[test]
    fn every_kiosk_route_is_one_the_edge_publishes() {
        let routes = include_str!("../../../../docs/snapshots/routes.txt");
        for (verb, path, _) in KIOSK_ROUTES {
            let line = format!("{verb} {path}");
            assert!(routes.lines().any(|published| published == line), "{line}");
        }
    }
}
