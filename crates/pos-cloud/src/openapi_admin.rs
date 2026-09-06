// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The generated OpenAPI document for the `/admin` console surface, and its drift gate
//! ([ADR-0019](../../../docs/adr/0019-openapi-generation.md), amended; roadmap v3 B5).
//!
//! # Why a second document
//!
//! [`crate::openapi`] is the **integrator** contract: three `/v1` routes an outside system calls
//! with a scoped API key. This is the **console** contract: the 136 `/admin` routes the dashboard
//! calls with a session cookie, which a fork needs when it writes its own console, a mobile admin
//! app, or an internal tool. Folding them into one document would bury the three routes an
//! integrator wants under a hundred and thirty-six they do not, so each surface gets its own
//! document with its own audience, title, and security scheme.
//!
//! # Fidelity: paths, parameters and statuses — not response schemas
//!
//! The documented shape is the **request** side and the outcomes: path, method, path and query
//! parameters, `If-Match` where a write is conditional, every status code the handler can answer,
//! and the [AIP-193 error envelope](ErrorResponse) those failures carry. Success bodies are
//! described in prose rather than as schemas.
//!
//! That is a deliberate stop, not an oversight. An `/admin` handler returns a **`pos-proto` wire
//! type** directly — [ADR-0079](../../../docs/adr/0079-inventory-and-suppliers.md) and its siblings
//! made the authored record *be* the wire type precisely so there is no second cloud-side shape to
//! keep in sync. Generating a schema from those types means deriving `utoipa::ToSchema` on them,
//! which means `utoipa` inside `pos-proto`: a backbone crate under the forbid-pass, governed by
//! `tools/backbone-allowlist.toml`, which the `deps-rule` gate requires an ADR to change. The
//! alternative — hand-mirroring forty wire types as local schema DTOs — reintroduces exactly the
//! duplication those ADRs removed.
//!
//! So this document tells a client *which routes exist, what to send, and what can come back*, and
//! stops short of the response schema. The error envelope is the **one** mirrored type, because it
//! is one type rather than forty and because it is the half of the contract a client must branch on;
//! [`the_mirrored_error_envelope_matches_the_real_one`] fails if the mirror drifts from
//! [`pos_proto::error::ErrorResponse`].
//!
//! # The gate
//!
//! Two things can rot. The document can drift from the code, which
//! [`the_committed_admin_openapi_matches_the_code`] catches the way `/v1`'s snapshot does. And a
//! route can be added without being documented, which [`every_admin_route_is_documented_or_listed`]
//! catches: it reads this crate's own router source, extracts every registered `/admin` path, and
//! requires each to be either in the document or named in [`UNDOCUMENTED`]. That list is the
//! coverage debt, in the repository, shrinking — and a route added tomorrow lands in neither and
//! fails the build.

use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
use utoipa::{Modify, OpenApi};

/// One reason a request failed, tied to the field responsible — the schema mirror of
/// [`pos_proto::error::ErrorDetail`].
#[derive(utoipa::ToSchema)]
#[expect(
    dead_code,
    reason = "a schema-only mirror: utoipa reads the field names and types to build the document, \
              and nothing constructs or reads an instance"
)]
pub(crate) struct ErrorDetail {
    /// The offending field, in the same `snake_case` name the request used. For a whole-request
    /// fault this can be a header name (`if-match`) rather than a body field.
    pub(crate) field: String,
    /// A stable, machine-readable reason such as `MUST_BE_POSITIVE` or `INVALID_ENUM_VALUE`. Stable
    /// so a client can branch on it; `message` is for people and may be reworded at any time.
    pub(crate) reason: String,
}

/// The error body — the schema mirror of [`pos_proto::error::ErrorBody`].
#[derive(utoipa::ToSchema)]
#[expect(dead_code, reason = "a schema-only mirror, as `ErrorDetail` explains")]
pub(crate) struct ErrorBody {
    /// The HTTP status code, repeated here so a body is self-describing once logged.
    pub(crate) code: u16,
    /// The canonical status, e.g. `INVALID_ARGUMENT`, `NOT_FOUND`, `FAILED_PRECONDITION`.
    pub(crate) status: String,
    /// A human-readable explanation. Never contains personal data, because responses are logged.
    pub(crate) message: String,
    /// Field-level detail. Absent when the failure is not about a particular field.
    pub(crate) details: Vec<ErrorDetail>,
}

/// The envelope every `/admin` failure carries — the schema mirror of
/// [`pos_proto::error::ErrorResponse`].
///
/// One level of nesting so a response is unambiguous at the top level: a success body is the record,
/// a failure body is `{"error": …}`.
#[derive(utoipa::ToSchema)]
#[expect(dead_code, reason = "a schema-only mirror, as `ErrorDetail` explains")]
pub(crate) struct ErrorResponse {
    /// The error.
    pub(crate) error: ErrorBody,
}

/// The `/admin` console API document.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Pizza 4P's POS Console API",
        version = "1.0.0",
        description = "The `/admin` surface the back-office console is built on. Generated from \
                       the handlers; see ADR-0019. Requests, parameters and outcomes are \
                       documented; success bodies are described in prose rather than as schemas \
                       (the module docstring explains why). Every failure carries the AIP-193 \
                       `ErrorResponse` envelope."
    ),
    paths(
        crate::http::admin_setup,
        crate::http::admin_login,
        crate::http::admin_logout,
        crate::http::admin_session,
        crate::http::admin_whoami,
        crate::http::admin_reenrol_totp,
        crate::http::admin_generate_recovery_codes,
        crate::http::admin_recovery_codes_status,
    ),
    components(schemas(ErrorResponse, ErrorBody, ErrorDetail)),
    modifiers(&SessionCookie),
    tags((
        name = "auth",
        description = "Two-factor sign-in, the session it issues, and the second-factor levers \
                       (ADR-0034, ADR-0060). Everything else on this surface stands behind the \
                       session these routes establish."
    ))
)]
pub(crate) struct AdminApiDoc;

/// Declares the host-only session cookie the `/admin` routes authenticate with
/// ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)), so the document tells a client how to
/// authenticate. A modifier rather than an attribute, for the reason `/v1`'s bearer scheme is one.
struct SessionCookie;

impl Modify for SessionCookie {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "session_cookie",
                SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                    crate::auth::session::COOKIE_NAME,
                    "The session cookie `POST /admin/login` sets: host-only, `HttpOnly`, \
                     `Secure`, `SameSite=Strict`, with a sliding idle TTL. A client does not read \
                     it — the browser presents it.",
                ))),
            );
        }
    }
}

/// The `/admin` routes this document does not yet describe — the coverage debt, and the reason
/// [`every_admin_route_is_documented_or_listed`] can pass while coverage is partial.
///
/// Every entry is a path a fork's client can call and will find no documentation for. The list only
/// shrinks: a route added without an entry and without an annotation fails that test, so new
/// surface cannot join the debt silently. When it reaches empty, delete it and the test becomes
/// "every route is documented" with nothing further to do.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the coverage debt is documentation that ships with the crate — the tests are what \
                  read it, but deleting it from a release build would delete the record"
    )
)]
const UNDOCUMENTED: &[&str] = &[
    // --- sessions and administrator management (G1, ADR-0067) ---
    "/admin/sessions",
    "/admin/sessions/revoke-others",
    "/admin/sessions/{id}",
    "/admin/admins",
    "/admin/admins/{id}/role",
    "/admin/admins/{id}/status",
    "/admin/invites",
    "/admin/invites/accept",
    "/admin/invites/{id}",
    // --- the tenant / brand / store registry (WS-C) ---
    "/admin/tenants",
    "/admin/tenants/{tenant_id}",
    "/admin/brands",
    "/admin/brands/{brand_id}",
    "/admin/stores",
    "/admin/stores/{store_id}",
    "/admin/stores/{store_id}/config",
    "/admin/stores/{store_id}/config/rollback",
    "/admin/stores/{store_id}/config/versions",
    "/admin/stores/{store_id}/config/versions/{version_id}",
    "/admin/stores/{store_id}/config/{level}",
    "/admin/stores/{store_id}/devices",
    "/admin/stores/{store_id}/devices/{device_id}",
    "/admin/stores/{store_id}/reports/xz",
    "/admin/stores/{store_id}/revenue/daily",
    "/admin/stores/{store_id}/revenue/export",
    "/admin/stores/{store_id}/rollups/daily",
    "/admin/stores/{store_id}/rollups/export",
    "/admin/stores/{store_id}/rollups/reset",
    // --- scoped integrator keys, webhook destinations, device onboarding ---
    "/admin/api-keys",
    "/admin/api-keys/{id}",
    "/admin/webhooks",
    "/admin/webhooks/{id}",
    "/admin/webhooks/{id}/enable",
    "/admin/devices/proposals",
    "/admin/devices/proposals/{id}/approve",
    "/admin/devices/proposals/{id}/reject",
    "/admin/activation-codes",
    "/admin/activation-codes/revoke",
    // --- the four-level configuration tree and its publishes (ADR-0033) ---
    "/admin/config/campaigns",
    "/admin/config/campaigns/preview",
    "/admin/config/campaigns/schedule",
    "/admin/config/capabilities",
    "/admin/config/channels",
    "/admin/config/inventory",
    // The store's lease (ADR-0108). `GET` reads the authoritative generation; `POST …/bump` issues
    // the next one, which is the act of making a different machine the store. There is no `PUT`,
    // deliberately: the node is derived from the counter, never authored.
    "/admin/config/lease",
    "/admin/config/lease/bump",
    "/admin/config/lease/retire",
    "/admin/config/lease/settle",
    "/admin/config/locale",
    "/admin/config/ota",
    "/admin/config/ota/halt",
    "/admin/config/ota/placement",
    "/admin/config/store-profile",
    // --- release hosting (R2, ADR-0088) ---
    // The upload's shape cannot be guessed — a raw executable body with minisign's signature line in
    // `X-Pos-Minisig` — so `docs/release-runbook.md` step 5 carries the exact `curl`, which is a
    // better home for it than a document that stops short of response schemas anyway. Listed here
    // rather than annotated to stay inside the fidelity B5 chose: the auth surface is documented, the
    // console surface is enumerated.
    "/admin/releases",
    "/admin/releases/{release}",
    "/admin/config/qr",
    "/admin/config/scheduled",
    "/admin/config/scheduled/{id}",
    "/admin/config/tax",
    "/admin/config/tender",
    "/admin/config/vendors",
    "/admin/capabilities",
    // --- the catalog: item master, menus, taxonomies, layout (F3, ADR-0066/0082) ---
    "/admin/catalog/display-categories",
    "/admin/catalog/display-categories/{display_category_id}",
    "/admin/catalog/display-subcategories",
    "/admin/catalog/display-subcategories/{display_subcategory_id}",
    "/admin/catalog/export/items",
    "/admin/catalog/item-categories",
    "/admin/catalog/item-categories/{item_category_id}",
    "/admin/catalog/item-subcategories",
    "/admin/catalog/item-subcategories/{item_subcategory_id}",
    "/admin/catalog/items",
    "/admin/catalog/items/{menu_item_id}",
    "/admin/catalog/layout-buttons",
    "/admin/catalog/layout-buttons/{sales_channel}/{menu_item_id}",
    "/admin/catalog/menus",
    "/admin/catalog/menus/{menu_id}",
    "/admin/catalog/menus/{menu_id}/placements",
    "/admin/catalog/menus/{menu_id}/placements/{menu_item_id}",
    "/admin/catalog/menus/{menu_id}/sections",
    "/admin/catalog/menus/{menu_id}/sections/{menu_section_id}",
    "/admin/catalog/modifier-groups",
    "/admin/catalog/modifier-groups/{modifier_group_id}",
    "/admin/catalog/publish",
    "/admin/catalog/tax-classes",
    "/admin/catalog/tax-classes/{tax_class_id}",
    "/admin/catalog/tax-rates",
    // --- people and access (M1, ADR-0070) ---
    "/admin/employees",
    "/admin/employees/{employee_id}",
    "/admin/employees/{employee_id}/pin",
    "/admin/people/permissions",
    "/admin/people/publish",
    "/admin/roles",
    "/admin/roles/{role_id}",
    "/admin/assignments",
    "/admin/assignments/{assignment_id}",
    // --- ingredients, recipes, suppliers (M6, ADR-0079) ---
    "/admin/inventory/ingredients",
    "/admin/inventory/ingredients/{ingredient_id}",
    "/admin/inventory/recipes",
    "/admin/inventory/recipes/{item_id}",
    "/admin/inventory/suppliers",
    "/admin/inventory/suppliers/{supplier_id}",
    // --- approved printers and kitchen displays (C2, ADR-0100) ---
    "/admin/devices/publish",
    // --- floor plan and kitchen stations (M2, ADR-0072) ---
    "/admin/floor/areas",
    "/admin/floor/areas/{area_id}",
    "/admin/floor/publish",
    "/admin/floor/qr",
    "/admin/floor/tables",
    "/admin/floor/tables/{table_id}",
    "/admin/kitchen/routing",
    "/admin/kitchen/routing/{rule_id}",
    "/admin/kitchen/stations",
    "/admin/kitchen/stations/{station_id}",
    // --- campaigns and vouchers (M3, ADR-0077) ---
    "/admin/campaigns",
    "/admin/campaigns/{campaign_id}",
    "/admin/campaigns/{campaign_id}/vouchers",
    // --- media library and the translation grid (M5, M4) ---
    "/admin/media",
    "/admin/media/{media_id}",
    "/admin/media/{media_id}/detail",
    "/admin/media/{media_id}/thumbnail",
    "/admin/translations",
    "/admin/translations/export",
    "/admin/translations/import/apply",
    "/admin/translations/import/dry-run",
    "/admin/countries",
    "/admin/locales",
    // --- operations: liveness, alerts, task health, OTA, reconciliation (O1-O3) ---
    "/admin/fleet",
    "/admin/fleet/{store_id}",
    "/admin/alerts",
    "/admin/alerts/{id}/ack",
    "/admin/alerts/{id}/resolve",
    "/admin/health/tasks",
    "/admin/reconcile",
    "/admin/audit",
    // --- PDPD subject-request tooling (ADR-0076) ---
    "/admin/subjects/{subject_id}",
    "/admin/subjects/{subject_id}/erase",
    "/admin/subjects/{subject_id}/export",
    // --- this document's own route ---
    "/admin/openapi.json",
];

/// Every `/admin` path the router registers, read out of this crate's own router source.
///
/// Axum's `Router` does not expose its routes, so the registered set is recovered from the source
/// the way the `xtask` gates read the tree: take each `.route(` and the next string literal after
/// it. That literal is the path, whether rustfmt kept it on the same line or wrapped it onto the
/// next.
///
/// Deliberately not a regex: the tree carries no regex dependency, and the grammar here is one
/// token wide.
#[cfg(test)]
fn registered_admin_paths() -> std::collections::BTreeSet<String> {
    /// The router source. Every `/admin` route is registered here — the other route-carrying
    /// modules (`orders`, `qr_http`, `relay`) serve `/v1` and the relay surface only, which
    /// `no_admin_routes_are_registered_outside_the_router_source` holds them to.
    const ROUTER_SOURCE: &str = include_str!("http.rs");

    let mut paths = std::collections::BTreeSet::new();
    for tail in ROUTER_SOURCE.split(".route(").skip(1) {
        let Some(open) = tail.find('"') else { continue };
        let after_open = tail.get(open + 1..).unwrap_or_default();
        let Some(close) = after_open.find('"') else {
            continue;
        };
        let literal = after_open.get(..close).unwrap_or_default();
        if literal == "/admin" || literal.starts_with("/admin/") {
            paths.insert(literal.to_owned());
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{AdminApiDoc, UNDOCUMENTED, registered_admin_paths};
    use utoipa::OpenApi as _;

    /// Where the generated document is committed, relative to the repository root.
    const SNAPSHOT_PATH: &str = "docs/openapi-admin.json";

    fn rendered() -> String {
        AdminApiDoc::openapi().to_pretty_json().unwrap_or_default()
    }

    /// Every path the document describes.
    fn documented_paths() -> BTreeSet<String> {
        AdminApiDoc::openapi().paths.paths.keys().cloned().collect()
    }

    /// The committed document is what the code generates — the same gate `/v1`'s document has.
    #[test]
    fn the_committed_admin_openapi_matches_the_code() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(SNAPSHOT_PATH);
        let rendered = rendered();
        if std::env::var("POS_UPDATE_SNAPSHOTS").is_ok() {
            std::fs::write(&path, &rendered).expect("write the console openapi document");
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_else(|_ignored| {
            panic!(
                "{SNAPSHOT_PATH} is missing. Regenerate it:\
                 \n    POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-cloud openapi_admin\n"
            )
        });
        assert_eq!(
            committed.trim(),
            rendered.trim(),
            "{SNAPSHOT_PATH} disagrees with the code. Regenerate it:\
             \n    POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-cloud openapi_admin\n"
        );
    }

    /// The mirrored error envelope has the same field names as the real one.
    ///
    /// The mirror exists because `pos-proto` cannot carry `utoipa` (see the module docstring), which
    /// makes it the one place in this document that can silently lie: rename a field on
    /// `pos_proto::error::ErrorBody` and the schema would still describe the old name. So compare
    /// against a real serialized envelope rather than trusting the two to stay aligned.
    #[test]
    fn the_mirrored_error_envelope_matches_the_real_one() {
        use pos_proto::error::ErrorStatus;

        let real = pos_proto::error::ErrorResponse::new(ErrorStatus::InvalidArgument, "a message")
            .with_detail("a_field", "A_REASON");
        let real = serde_json::to_value(&real).expect("serialize the real envelope");

        let document = AdminApiDoc::openapi();
        let json = serde_json::to_value(&document).expect("serialize the document");
        let schemas = &json["components"]["schemas"];

        for (schema, value) in [
            ("ErrorResponse", &real),
            ("ErrorBody", &real["error"]),
            ("ErrorDetail", &real["error"]["details"][0]),
        ] {
            let described: BTreeSet<String> = schemas[schema]["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("{schema} should describe its properties"))
                .keys()
                .cloned()
                .collect();
            let actual: BTreeSet<String> = value
                .as_object()
                .unwrap_or_else(|| panic!("the real {schema} should be an object"))
                .keys()
                .cloned()
                .collect();
            assert_eq!(
                described, actual,
                "the {schema} schema mirror has drifted from pos-proto's type",
            );
        }
    }

    /// Every registered `/admin` route is either documented or named as coverage debt.
    ///
    /// This is the half a snapshot cannot see: the snapshot proves the document matches the
    /// annotations, not that the annotations cover the router. Adding a route without either an
    /// annotation or an `UNDOCUMENTED` entry fails here, so the surface cannot grow undocumented
    /// without someone writing it down.
    #[test]
    fn every_admin_route_is_documented_or_listed() {
        let registered = registered_admin_paths();
        assert!(
            registered.len() > 100,
            "the route extractor found only {} paths, which means it stopped matching the source \
             rather than that the surface shrank",
            registered.len(),
        );
        let documented = documented_paths();
        let listed: BTreeSet<String> = UNDOCUMENTED.iter().map(|path| (*path).to_owned()).collect();

        let missing: Vec<&String> = registered
            .iter()
            .filter(|path| !documented.contains(*path) && !listed.contains(*path))
            .collect();
        assert!(
            missing.is_empty(),
            "these registered /admin routes are neither documented nor listed as coverage debt in \
             UNDOCUMENTED — annotate the handler with #[utoipa::path] or add the path to that \
             list:\n{missing:#?}",
        );
    }

    /// The document describes no path the router does not serve.
    ///
    /// A hand-edited `path = "…"` in an annotation is how a document comes to promise a route that
    /// does not exist, which is worse than an undocumented one: a client writes against it and gets
    /// a 404.
    #[test]
    fn the_document_promises_no_route_the_router_does_not_serve() {
        let registered = registered_admin_paths();
        let phantom: Vec<String> = documented_paths()
            .into_iter()
            .filter(|path| !registered.contains(path))
            .collect();
        assert!(
            phantom.is_empty(),
            "these paths are documented but not registered in the router — the annotation's \
             `path = ` disagrees with its `.route(` :\n{phantom:#?}",
        );
    }

    /// `UNDOCUMENTED` holds nothing that is documented, and nothing the router does not serve.
    ///
    /// Without this the list would rot in both directions: an entry for a route since annotated
    /// would hide it from the coverage count, and an entry for a route since deleted would sit
    /// there forever looking like debt.
    #[test]
    fn the_coverage_debt_list_holds_only_real_undocumented_routes() {
        let documented = documented_paths();
        let registered = registered_admin_paths();
        for path in UNDOCUMENTED {
            assert!(
                !documented.contains(*path),
                "{path} is documented — remove it from UNDOCUMENTED",
            );
            assert!(
                registered.contains(*path),
                "{path} is in UNDOCUMENTED but the router does not serve it — remove the entry",
            );
        }
        let unique: BTreeSet<&&str> = UNDOCUMENTED.iter().collect();
        assert_eq!(
            unique.len(),
            UNDOCUMENTED.len(),
            "UNDOCUMENTED lists a path twice",
        );
    }
}
