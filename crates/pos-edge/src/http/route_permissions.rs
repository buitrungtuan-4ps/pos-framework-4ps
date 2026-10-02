// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What each state-changing edge route needs
//! ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
//! decision 2).
//!
//! Every route that changes something names the permission a signed-in person must hold, or says
//! why no person acts through it. The table is the record; the checks themselves stay where they
//! are, in `pos_core::decision` and in the edge command each route calls, because a route can need
//! a different permission for different things it acts on (a line that fired, an amount above the
//! ceiling).
//!
//! A test holds the table to the published routes in `docs/snapshots/routes.txt`. A new
//! state-changing route that is not in the table fails CI, and so does an entry for a route that
//! no longer exists. A second list, [`NOT_ON_A_ROUTE`], names the catalogue's permissions no edge
//! route checks yet, so every permission is either checked by a route or listed as not yet.
//!
//! A read route (`GET`) is here only where a permission decides who may see what it shows: the
//! day's takings
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 2). Every other read changes nothing, and the signed-in gate covers it.

use pos_core::permission::Permission;

/// What stands between a request and a route's effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteGate {
    /// A signed-in person holding one of these. Which one depends on what the request acts on,
    /// and a route that lists several may need more than one at once (a transfer seats the table
    /// it moves to).
    Person(&'static [Permission]),
    /// No person acts through the route, and the text says what gates it instead: it is how a
    /// device or a person comes to hold a credential, or a device's own errand.
    NoPerson(&'static str),
}

/// Every state-changing edge route, and every read a permission gates, and what each needs, keyed
/// `METHOD /path` as `docs/snapshots/routes.txt` writes it, in that file's order.
pub const ROUTE_PERMISSIONS: &[(&str, RouteGate)] = &[
    (
        "GET /api/reports/takings",
        RouteGate::Person(&[Permission::ViewTakings]),
    ),
    (
        "POST /api/activate",
        RouteGate::NoPerson(
            "an activation code brings a new box online, before anyone can sign in on it (ADR-0143)",
        ),
    ),
    (
        "POST /api/bills/{id}/check/print",
        RouteGate::Person(&[Permission::OpenBill]),
    ),
    (
        "POST /api/bills/{id}/discount",
        RouteGate::Person(&[
            Permission::ApplyDiscount,
            Permission::OverrideDiscountCeiling,
        ]),
    ),
    (
        "POST /api/bills/{id}/fees/{fee_id}/waive",
        RouteGate::Person(&[Permission::WaiveFee]),
    ),
    (
        "POST /api/bills/{id}/merge",
        RouteGate::Person(&[Permission::SplitBill]),
    ),
    (
        "POST /api/bills/{id}/receipt/reprint",
        RouteGate::Person(&[Permission::ReprintReceipt]),
    ),
    (
        "POST /api/bills/{id}/settle",
        RouteGate::Person(&[Permission::TakePayment]),
    ),
    (
        "POST /api/bills/{id}/split",
        RouteGate::Person(&[Permission::SplitBill]),
    ),
    (
        "POST /api/bills/{id}/void",
        RouteGate::Person(&[Permission::VoidBill]),
    ),
    (
        "POST /api/drawer/open",
        RouteGate::Person(&[Permission::OpenDrawerNoSale]),
    ),
    (
        "POST /api/kds/bump",
        RouteGate::Person(&[Permission::BumpTicket]),
    ),
    (
        "POST /api/lines/{id}/fire",
        RouteGate::Person(&[Permission::FireLines]),
    ),
    (
        "POST /api/lines/{id}/quantity",
        RouteGate::Person(&[Permission::AddLine]),
    ),
    (
        "POST /api/lines/{id}/void",
        RouteGate::Person(&[Permission::AddLine, Permission::VoidFiredLine]),
    ),
    (
        "POST /api/menu/{id}/restore",
        RouteGate::Person(&[Permission::MarkItemUnavailable]),
    ),
    (
        "POST /api/menu/{id}/sold-out",
        RouteGate::Person(&[Permission::MarkItemUnavailable]),
    ),
    (
        "POST /api/orders",
        RouteGate::Person(&[Permission::AddLine]),
    ),
    (
        "POST /api/orders/{id}/bill",
        RouteGate::Person(&[Permission::OpenBill]),
    ),
    (
        "POST /api/orders/{id}/check/print",
        RouteGate::Person(&[Permission::OpenBill]),
    ),
    (
        "POST /api/orders/{id}/confirm",
        RouteGate::Person(&[Permission::ConfirmQrOrder]),
    ),
    (
        "POST /api/orders/{id}/fire",
        RouteGate::Person(&[Permission::FireLines]),
    ),
    (
        "POST /api/orders/{id}/fire/{course_id}",
        RouteGate::Person(&[Permission::FireLines]),
    ),
    (
        "POST /api/orders/{id}/lines",
        RouteGate::Person(&[Permission::AddLine]),
    ),
    (
        "POST /api/orders/{id}/reject",
        RouteGate::Person(&[Permission::ConfirmQrOrder]),
    ),
    (
        "POST /api/pair",
        RouteGate::NoPerson("a pairing code a manager minted, spent once (ADR-0030)"),
    ),
    (
        "POST /api/pair/codes",
        RouteGate::Person(&[Permission::ManageDevices]),
    ),
    (
        "POST /api/pair/revoke",
        RouteGate::Person(&[Permission::ManageDevices]),
    ),
    (
        "POST /api/print/agent",
        RouteGate::Person(&[Permission::ManageDevices]),
    ),
    (
        "POST /api/print/agent/revoke",
        RouteGate::Person(&[Permission::ManageDevices]),
    ),
    (
        "POST /api/print/jobs/{job_id}/ack",
        RouteGate::NoPerson("a bound print agent reporting a job it wrote (ADR-0112)"),
    ),
    (
        "POST /api/printers/{id}/test",
        RouteGate::Person(&[Permission::ManageDevices]),
    ),
    (
        "POST /api/session/sign-in",
        RouteGate::NoPerson("a staff code and PIN: where a person becomes the one acting"),
    ),
    (
        "POST /api/session/sign-out",
        RouteGate::NoPerson("ends this device's own sign-in"),
    ),
    (
        "POST /api/shifts",
        RouteGate::Person(&[Permission::OpenShift]),
    ),
    (
        "POST /api/shifts/{id}/close",
        RouteGate::Person(&[Permission::CloseShift]),
    ),
    (
        "POST /api/shifts/{id}/count",
        RouteGate::Person(&[Permission::CloseShift]),
    ),
    (
        "POST /api/shifts/{id}/paid-in",
        RouteGate::Person(&[Permission::RecordCashMovement]),
    ),
    (
        "POST /api/shifts/{id}/paid-out",
        RouteGate::Person(&[Permission::RecordCashMovement]),
    ),
    (
        "POST /api/tables/{id}/bill",
        RouteGate::Person(&[Permission::OpenBill]),
    ),
    (
        "POST /api/tables/{id}/check/print",
        RouteGate::Person(&[Permission::OpenBill]),
    ),
    (
        "POST /api/tables/{id}/clean",
        RouteGate::Person(&[Permission::ManageTables]),
    ),
    (
        "POST /api/tables/{id}/lines",
        RouteGate::Person(&[Permission::AddLine]),
    ),
    (
        "POST /api/tables/{id}/release",
        RouteGate::Person(&[Permission::ManageTables]),
    ),
    (
        "POST /api/tables/{id}/seat",
        RouteGate::Person(&[Permission::ManageTables]),
    ),
    (
        "POST /api/tables/{id}/transfer",
        RouteGate::Person(&[Permission::TransferOrder, Permission::ManageTables]),
    ),
];

/// The catalogue's permissions no edge route checks yet, each with why.
///
/// Not a gap in the table: these are acts the till does not offer yet, or acts that happen in the
/// console rather than at a store. A permission moves out of this list in the change that gives it
/// a route.
pub const NOT_ON_A_ROUTE: &[(Permission, &str)] = &[
    (
        Permission::AddOpenItem,
        "the till cannot ring an open item yet",
    ),
    (
        Permission::OverridePrice,
        "the till cannot override a price yet",
    ),
    (
        Permission::ApplyComp,
        "the till cannot comp a bill yet; the discount route only discounts",
    ),
    (
        Permission::IssueRefund,
        "refunds are not built yet (ADR-0028)",
    ),
    (
        Permission::PerformStocktake,
        "stocktakes are not built at the store yet",
    ),
    (
        Permission::RecordStockReceipt,
        "stock receipts are not built at the store yet",
    ),
    (
        Permission::RecordWaste,
        "no route records waste on its own; a fired line's void records it",
    ),
    (
        Permission::EditMenu,
        "the menu is authored in the console and published",
    ),
    (
        Permission::EditStoreConfig,
        "a store's configuration is authored in the console and published",
    ),
    (Permission::ManageTenant, "cloud-only administration"),
    (Permission::ManageStaff, "cloud-only administration"),
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use pos_core::permission::Permission;

    use super::{NOT_ON_A_ROUTE, ROUTE_PERMISSIONS, RouteGate};

    /// Every route the edge publishes, from the committed route snapshot.
    fn published_routes() -> BTreeSet<String> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/snapshots/routes.txt");
        let text = std::fs::read_to_string(&path).expect("the route snapshot is committed");
        text.lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// Every route the edge publishes that is not a read.
    fn state_changing_routes() -> BTreeSet<String> {
        published_routes()
            .into_iter()
            .filter(|line| !line.starts_with("GET "))
            .collect()
    }

    #[test]
    fn every_state_changing_route_says_what_it_needs() {
        let tabled: BTreeSet<String> = ROUTE_PERMISSIONS
            .iter()
            .map(|(route, _gate)| (*route).to_owned())
            .collect();
        assert_eq!(
            tabled.len(),
            ROUTE_PERMISSIONS.len(),
            "a route is listed once"
        );
        let untabled: Vec<String> = state_changing_routes()
            .difference(&tabled)
            .cloned()
            .collect();
        assert!(
            untabled.is_empty(),
            "a state-changing route must name the permission it needs (ADR-0158) — add these to \
             ROUTE_PERMISSIONS: {untabled:?}"
        );
        let gone: Vec<String> = tabled.difference(&published_routes()).cloned().collect();
        assert!(
            gone.is_empty(),
            "ROUTE_PERMISSIONS lists routes the edge does not publish: {gone:?}"
        );
    }

    /// A read is in the table only because a permission gates who may see it, so it names one.
    #[test]
    fn a_tabled_read_is_gated_by_a_permission() {
        for (route, gate) in ROUTE_PERMISSIONS {
            if route.starts_with("GET ") {
                assert!(
                    matches!(gate, RouteGate::Person(permissions) if !permissions.is_empty()),
                    "{route} is a read, so it is here only to name the permission that gates it"
                );
            }
        }
    }

    #[test]
    fn a_person_route_names_at_least_one_permission_and_a_no_person_route_says_why() {
        for (route, gate) in ROUTE_PERMISSIONS {
            match gate {
                RouteGate::Person(permissions) => {
                    assert!(!permissions.is_empty(), "{route} names a permission");
                }
                RouteGate::NoPerson(why) => {
                    assert!(!why.trim().is_empty(), "{route} says what gates it");
                }
            }
        }
    }

    #[test]
    fn every_permission_is_checked_by_a_route_or_listed_as_not_yet() {
        let routed: BTreeSet<Permission> = ROUTE_PERMISSIONS
            .iter()
            .filter_map(|(_route, gate)| match gate {
                RouteGate::Person(permissions) => Some(permissions.iter().copied()),
                RouteGate::NoPerson(_) => None,
            })
            .flatten()
            .collect();
        let not_yet: BTreeSet<Permission> = NOT_ON_A_ROUTE
            .iter()
            .map(|(permission, _why)| *permission)
            .collect();
        for permission in Permission::ALL {
            let id = permission.meta().id;
            assert!(
                routed.contains(permission) != not_yet.contains(permission),
                "{id} must be either checked by a route or listed in NOT_ON_A_ROUTE, not both and \
                 not neither"
            );
        }
    }
}
