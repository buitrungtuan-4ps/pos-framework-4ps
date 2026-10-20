// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The guarded domain routes, one sub-router per area, which [`super::domain_router`] merges behind
//! its two gates.
//!
//! Each area's routes are registered exactly as they were in one flat table; axum matches a request
//! by its path and method whichever sub-router registered it, and the gates are layered over the
//! merged router, so splitting the table changes no route, no precedence and no layer.

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::SubjectStore;

use super::revalidated;
use super::{
    bills, check, floor, integrations, kds, layout, lines, live, locale, menu, printers, qr,
    reason_codes, reports, shifts, sync, tables,
};
use crate::app::Edge;

/// Every guarded route, before the signed-in gate and the edge they share.
pub(super) fn guarded_routes<S>() -> Router<Arc<Edge<S>>>
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    Router::new()
        .merge(published_reads::<S>())
        .merge(floor_routes::<S>())
        .merge(order_routes::<S>())
        .merge(bill_routes::<S>())
        .merge(shift_routes::<S>())
        .merge(store_routes::<S>())
}

/// What the store published, read for the till, and the 86 that marks an item sold out.
fn published_reads<S>() -> Router<Arc<Edge<S>>>
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    Router::new()
        // The store's published floor plan + kitchen stations, for the UI to render real tables and
        // route fires (ADR-0072).
        .route("/api/floor", revalidated(get(floor::plan::<S>)))
        // The store's published price book, so the till prices from what the console published
        // rather than from a list compiled into the app (roadmap-v3 E5, ADR-0063).
        .route("/api/menu", revalidated(get(menu::catalog::<S>)))
        // Staff mark an item sold out, and bring it back (86). The events have been in the schema
        // since it was written; this is the first thing to emit them.
        .route("/api/menu/{id}/sold-out", post(menu::sold_out::<S>))
        .route("/api/menu/{id}/restore", post(menu::restore::<S>))
        // How the till groups and orders those items, from the `layout` node the same publish writes
        // (ADR-0066, production-readiness C4). A separate node, so a separate route: a price change
        // relays no buttons and a button moving reprices nothing.
        .route("/api/layout", revalidated(get(layout::plan::<S>)))
        // The money facts the pay pad needs — currency, the notes a guest carries, what the total
        // rounds to in cash. A country's coinage, published rather than compiled in (ADR-0105).
        .route("/api/locale", revalidated(get(locale::settings::<S>)))
        // The store's managed reason list, so a picker offers what the store actually holds
        // (ADR-0115). One read for every picker: each entry carries the actions it covers, and the
        // till filters by the act in hand.
        .route(
            "/api/reason-codes",
            revalidated(get(reason_codes::list::<S>)),
        )
}

/// The floor and what is open on it: tables, the check for a table or an order, and the live orders.
fn floor_routes<S>() -> Router<Arc<Edge<S>>>
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    Router::new()
        // The floor: seat, clean, move the guests to another table, read.
        .route("/api/tables/{id}/seat", post(tables::seat::<S>))
        .route("/api/tables/{id}/clean", post(tables::clean::<S>))
        .route("/api/tables/{id}/release", post(tables::release::<S>))
        .route("/api/tables/{id}/transfer", post(tables::transfer::<S>))
        .route("/api/tables/{id}", get(tables::get::<S>))
        // What the table owes right now, assembled by the edge — the till displays the figure it is
        // going to settle against rather than computing one of its own (roadmap-v3 E5).
        .route("/api/tables/{id}/check", get(check::read::<S>))
        // The same figures on paper, before payment: a pre-bill, one per open part once a table is
        // split, and unnumbered because nothing has been settled (roadmap-v3 B2.1).
        .route(
            "/api/tables/{id}/check/print",
            post(check::print_for_table::<S>),
        )
        // The same read keyed on the order, for a counter order that sits on no table (ADR-0093).
        .route("/api/orders/{id}/check", get(check::read_for_order::<S>))
        .route(
            "/api/orders/{id}/check/print",
            post(check::print_for_order::<S>),
        )
        // What is open right now, with the line ids to act on it. A device learns lines from the
        // fan-out, which carries what happens next — so without this read a till that reloads and a
        // kitchen display switched on mid-service both draw an empty screen over live food.
        .route("/api/orders/live", get(live::read::<S>))
        .route("/api/orders/kitchen", get(live::kitchen::<S>))
}

/// The order: its lines, firing them, the staff-confirmation queue, and the kitchen's bump.
fn order_routes<S>() -> Router<Arc<Edge<S>>>
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    Router::new()
        // The order: add a line to a table, fire a line to the kitchen.
        .route("/api/tables/{id}/lines", post(lines::add::<S>))
        .route("/api/lines/{id}/fire", post(lines::fire::<S>))
        // How many, while the line is still editable. The till could only ever add one of a thing,
        // so "three beers" was three lines.
        .route("/api/lines/{id}/quantity", post(lines::set_quantity::<S>))
        // And the whole order in one tap. Firing line by line cost an operator one tap per line and
        // left half an order with the kitchen when one of them failed; this commits them together.
        .route("/api/orders/{id}/fire", post(lines::fire_order::<S>))
        // And one course of it, which is what `courses_enabled` gates (ADR-0130). The narrowing of
        // the act above, not a different one: same transaction boundary, same routing, same gate.
        .route(
            "/api/orders/{id}/fire/{course_id}",
            post(lines::fire_course::<S>),
        )
        // The staff-confirmation queue (ADR-0116). A guest's tabled QR order cannot be fired until
        // one of these two decisions lands, which is the guardrail ADR-0012 promised.
        .route("/api/orders/awaiting-confirmation", get(qr::awaiting::<S>))
        .route("/api/orders/{id}/confirm", post(qr::confirm::<S>))
        .route("/api/orders/{id}/reject", post(qr::reject::<S>))
        // The kitchen display: bump a ticket (mark lines prepared), durable and fanned out.
        .route("/api/kds/bump", post(kds::bump::<S>))
}

/// The bill: opening, settling and reprinting it, its check, voids, reductions, and split and merge.
fn bill_routes<S>() -> Router<Arc<Edge<S>>>
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    Router::new()
        // The bill: open on a table, open on an order, settle. The order-keyed route is what makes
        // a takeaway order chargeable — it has no table to open a bill against (ADR-0093).
        .route("/api/tables/{id}/bill", post(bills::open::<S>))
        .route("/api/orders/{id}/bill", post(bills::open_for_order::<S>))
        .route("/api/bills/{id}/settle", post(bills::settle::<S>))
        .route("/api/bills/{id}/receipt/reprint", post(bills::reprint::<S>))
        // What one bill owes and the lines it covers — the read a split needs, because once a
        // table's bill is split each guest at the till is asking about their own part (ADR-0128).
        .route("/api/bills/{id}/check", get(check::read_for_bill::<S>))
        .route(
            "/api/bills/{id}/check/print",
            post(check::print_for_bill::<S>),
        )
        // The void pair (ADR-0115, roadmap B2.2). A fired line and any bill need a manager's
        // PIN with the request, which is the first thing in the tree to read the permission
        // registry's `pin: true`.
        .route("/api/lines/{id}/void", post(lines::void::<S>))
        .route("/api/bills/{id}/void", post(bills::void::<S>))
        .route("/api/bills/{id}/discount", post(bills::discount::<S>))
        // Waiving one of the bill's fees (ADR-0159 decision 5): `billing.fee.waive`, PIN-flagged,
        // so the approver rides with the request as it does for a void.
        .route(
            "/api/bills/{id}/fees/{fee_id}/waive",
            post(bills::waive_fee::<S>),
        )
        // Split and merge (ADR-0128). Both need `billing.bill.split` (ADR-0158) and neither asks
        // for a PIN: both move amounts that are already captured, and a prompt here would be paid
        // for on every table.
        .route("/api/bills/{id}/split", post(bills::split::<S>))
        .route("/api/bills/{id}/merge", post(bills::merge::<S>))
}

/// The cash shifts and their drawers.
fn shift_routes<S>() -> Router<Arc<Edge<S>>>
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    Router::new()
        // The cash shift: open, blind count, close — and the one that is open, so a device that
        // reloads mid-shift can still count and close it rather than offering to open another.
        .route("/api/shifts/current", get(shifts::current::<S>))
        // Every drawer the store keeps, one per till where it keeps one per till (ADR-0167).
        .route(
            "/api/shifts",
            get(shifts::drawers::<S>).post(shifts::open::<S>),
        )
        .route("/api/shifts/{id}/count", post(shifts::count::<S>))
        .route("/api/shifts/{id}/close", post(shifts::close::<S>))
        // Several counted drawers closed at once, a custom method on the collection (ADR-0167).
        .route("/api/shifts:batch_close", post(shifts::batch_close::<S>))
        .route("/api/shifts/{id}/paid-in", post(shifts::paid_in::<S>))
        .route("/api/shifts/{id}/paid-out", post(shifts::paid_out::<S>))
        .route("/api/drawer/open", post(shifts::open_drawer::<S>))
}

/// The store's link, takings, integrations and printers.
fn store_routes<S>() -> Router<Arc<Edge<S>>>
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    Router::new()
        // The cloud link and the outbox, for the status bar (ADR-0137): "Offline — selling
        // normally" and how many events are waiting.
        .route("/api/sync", get(sync::read::<S>))
        // What the store has taken today, for the Today screen's tile (ADR-0160 decision 2), to a
        // person whose own role grants `reports.takings.view`, checked in the handler.
        .route("/api/reports/takings", get(reports::takings::<S>))
        // The published printers, and a manager's test page on one — how a new store learns a
        // printer is wired before a guest's receipt tells it.
        // The vendor connections the cloud published to this store (ADR-0153): which e-invoice
        // provider, card terminal or courier it is set up for. Names only, never a setting.
        .route("/api/integrations", get(integrations::list::<S>))
        .route("/api/printers", get(printers::list::<S>))
        .route("/api/printers/{id}/test", post(printers::test::<S>))
}
