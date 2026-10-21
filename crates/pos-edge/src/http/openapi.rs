// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The till's OpenAPI document, `docs/openapi-edge.json`
//! ([ADR-0177](../../../../docs/adr/0177-openapi-is-the-route-map.md)).
//!
//! Compiled only with the `openapi` feature, which this crate's own dev-dependency on itself turns
//! on, so `cargo test` builds and checks the document and a store's binary compiles none of it
//! (decision 2). The edge does not serve the document: it is committed, not a route.
//!
//! The registered routes are the ones `docs/snapshots/routes.txt` publishes, plus `/healthz` and
//! `/ws`. Each is either documented here or named in [`UNDOCUMENTED`], the coverage debt, which
//! only shrinks: a route added without either fails [`tests`], and so does a documented path no
//! router serves, an entry in the list that is documented or gone, and an `operationId` used twice.

use utoipa::OpenApi;

/// The till's API document. Only its tests read it, since the edge does not serve it.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the document is committed by its test, never served by the edge"
    )
)]
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Pizza 4P's POS Till API",
        version = "1.0.0",
        description = "The `/api` surface the till, the kitchen display and POS Station call on a \
                       store's edge, generated from the handlers (ADR-0177). A device presents its \
                       pairing token as a bearer token, and most routes also need a person signed \
                       in on it. A refusal is plain text, with its AIP-193 reason in the \
                       `pos-error-reason` header where the edge names one."
    ),
    paths(
        super::health::healthz,
        super::ws::fan_out,
        super::pair::pair,
        super::pair::this_device,
        super::pair::paired_devices,
        super::auth::current_session,
        super::auth::sign_in,
        super::auth::sign_out,
        super::floor::floor_plan,
        super::menu::menu_catalog,
        super::layout::layout_plan,
        super::locale::locale_settings,
        super::reason_codes::list_reason_codes,
        super::integrations::list_integrations,
        super::printers::list_printers,
        super::sync::sync_status,
        super::reports::takings,
    ),
    tags(
        (name = "pairing", description = "How a device comes to hold a token, and whether it still \
                                          does (ADR-0030, ADR-0091)."),
        (name = "session", description = "Who is signed in on a paired device (ADR-0084, ADR-0158)."),
        (name = "published configuration", description = "What the cloud published to this store, \
                                          read from the live session. A configuration read \
                                          answers 304 to an If-None-Match that names its ETag \
                                          (ADR-0173)."),
        (name = "status", description = "The box's own state: liveness, the outbox and the cloud \
                                         link, today's takings, and the live fan-out."),
    )
)]
pub(crate) struct EdgeApiDoc;

/// The routes this document does not yet describe, as `routes.txt` writes them: the coverage debt,
/// which only shrinks.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the coverage debt is documentation that ships with the crate; the tests read it"
    )
)]
const UNDOCUMENTED: &[&str] = &[
    "GET /api/activation",
    "GET /api/bills/settled",
    "GET /api/bills/{id}/check",
    "GET /api/claim",
    "GET /api/orders/awaiting-confirmation",
    "GET /api/orders/kitchen",
    "GET /api/orders/live",
    "GET /api/orders/open",
    "GET /api/orders/{id}/check",
    "GET /api/print/agent",
    "GET /api/print/jobs",
    "GET /api/shifts",
    "GET /api/shifts/current",
    "GET /api/tables/{id}",
    "GET /api/tables/{id}/check",
    "POST /api/activate",
    "POST /api/bills/{id}/check/print",
    "POST /api/bills/{id}/discount",
    "POST /api/bills/{id}/fees/{fee_id}/waive",
    "POST /api/bills/{id}/merge",
    "POST /api/bills/{id}/receipt/reprint",
    "POST /api/bills/{id}/settle",
    "POST /api/bills/{id}/split",
    "POST /api/bills/{id}/void",
    "POST /api/drawer/open",
    "POST /api/kds/bump",
    "POST /api/lines/{id}/fire",
    "POST /api/lines/{id}/quantity",
    "POST /api/lines/{id}/void",
    "POST /api/menu/{id}/restore",
    "POST /api/menu/{id}/sold-out",
    "POST /api/orders",
    "POST /api/orders/{id}/bill",
    "POST /api/orders/{id}/check/print",
    "POST /api/orders/{id}/confirm",
    "POST /api/orders/{id}/fire",
    "POST /api/orders/{id}/fire/{course_id}",
    "POST /api/orders/{id}/lines",
    "POST /api/orders/{id}/reject",
    "POST /api/pair/codes",
    "POST /api/pair/revoke",
    "POST /api/print/agent",
    "POST /api/print/agent/revoke",
    "POST /api/print/jobs/{job_id}/ack",
    "POST /api/printers/{id}/test",
    "POST /api/shifts",
    "POST /api/shifts/{id}/close",
    "POST /api/shifts/{id}/count",
    "POST /api/shifts/{id}/paid-in",
    "POST /api/shifts/{id}/paid-out",
    "POST /api/shifts:batch_close",
    "POST /api/tables/{id}/bill",
    "POST /api/tables/{id}/check/print",
    "POST /api/tables/{id}/clean",
    "POST /api/tables/{id}/lines",
    "POST /api/tables/{id}/release",
    "POST /api/tables/{id}/seat",
    "POST /api/tables/{id}/transfer",
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::{EdgeApiDoc, UNDOCUMENTED};
    use utoipa::OpenApi as _;

    /// Where the generated document is committed, relative to the repository root.
    const SNAPSHOT_PATH: &str = "docs/openapi-edge.json";

    /// Every route the edge registers, `METHOD /path`: what `routes.txt` publishes, plus the two
    /// routes ADR-0111 leaves out of it.
    fn registered() -> BTreeSet<String> {
        let published =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/snapshots/routes.txt");
        let text = std::fs::read_to_string(published).expect("read docs/snapshots/routes.txt");
        let mut routes: BTreeSet<String> = text
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
            .map(str::to_owned)
            .collect();
        routes.insert("GET /healthz".to_owned());
        routes.insert("GET /ws".to_owned());
        routes
    }

    /// Every operation the document describes, `METHOD /path`, with its `operationId`.
    fn documented() -> Vec<(String, String)> {
        let json = serde_json::to_value(EdgeApiDoc::openapi()).expect("serialize the document");
        let mut operations = Vec::new();
        for (path, item) in json["paths"].as_object().into_iter().flatten() {
            for (method, operation) in item.as_object().into_iter().flatten() {
                let id = operation["operationId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                operations.push((format!("{} {path}", method.to_uppercase()), id));
            }
        }
        operations
    }

    /// The committed document is what the code generates.
    #[test]
    fn the_committed_edge_openapi_matches_the_code() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(SNAPSHOT_PATH);
        let rendered = EdgeApiDoc::openapi().to_pretty_json().unwrap_or_default();
        if std::env::var("POS_UPDATE_SNAPSHOTS").is_ok() {
            std::fs::write(&path, &rendered).expect("write the till openapi document");
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed.trim(),
            rendered.trim(),
            "{SNAPSHOT_PATH} disagrees with the code. Regenerate it:\
             \n    POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-edge openapi\n"
        );
    }

    /// Every registered route is documented or named as coverage debt.
    #[test]
    fn every_edge_route_is_documented_or_listed() {
        let documented: BTreeSet<String> =
            documented().into_iter().map(|(route, _)| route).collect();
        let listed: BTreeSet<String> = UNDOCUMENTED
            .iter()
            .map(|route| (*route).to_owned())
            .collect();
        let missing: Vec<String> = registered()
            .into_iter()
            .filter(|route| !documented.contains(route) && !listed.contains(route))
            .collect();
        assert!(
            missing.is_empty(),
            "annotate these routes' handlers, or list them in UNDOCUMENTED:\n{missing:#?}"
        );
    }

    /// The document promises no route the edge does not serve.
    #[test]
    fn the_document_promises_no_route_the_edge_does_not_serve() {
        let registered = registered();
        let phantom: Vec<String> = documented()
            .into_iter()
            .map(|(route, _)| route)
            .filter(|route| !registered.contains(route))
            .collect();
        assert!(
            phantom.is_empty(),
            "documented but not registered:\n{phantom:#?}"
        );
    }

    /// `UNDOCUMENTED` holds only routes that are registered and not documented, each once.
    #[test]
    fn the_coverage_debt_list_holds_only_real_undocumented_routes() {
        let registered = registered();
        let documented: BTreeSet<String> =
            documented().into_iter().map(|(route, _)| route).collect();
        for route in UNDOCUMENTED {
            assert!(
                !documented.contains(*route),
                "{route} is documented: remove it from UNDOCUMENTED"
            );
            assert!(
                registered.contains(*route),
                "{route} is not registered: remove it from UNDOCUMENTED"
            );
        }
        let unique: BTreeSet<&&str> = UNDOCUMENTED.iter().collect();
        assert_eq!(
            unique.len(),
            UNDOCUMENTED.len(),
            "UNDOCUMENTED lists a route twice"
        );
    }

    /// Each `operationId` is its handler's name, so two handlers may not share one (ADR-0177
    /// decision 4).
    #[test]
    fn every_operation_id_is_used_once() {
        let mut seen = BTreeSet::new();
        for (route, id) in documented() {
            assert!(!id.is_empty(), "{route} has no operationId");
            assert!(
                seen.insert(id.clone()),
                "{route} reuses the operationId {id}: rename a handler"
            );
        }
    }
}
