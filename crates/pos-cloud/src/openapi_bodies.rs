// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Every body the cloud's documents describe names its schema
//! ([ADR-0177](../../../docs/adr/0177-openapi-is-the-route-map.md) decision 3).
//!
//! A success answer with a body, and a request body, each carry a schema, except on the routes the
//! two lists below still name. The lists only shrink: a route leaves when its handler's annotation
//! names its bodies, and a list goes when it is empty. The `/v1` document has no list: every one
//! of its bodies has a schema.

use std::collections::BTreeSet;

use utoipa::OpenApi as _;

use crate::openapi::ApiDoc;
use crate::openapi_admin::AdminApiDoc;
use crate::openapi_device::DeviceApiDoc;

/// `/admin` routes, `METHOD /path`, with a success answer or a request body that has no schema yet.
const ADMIN_NO_SCHEMA_YET: &[&str] = &[
    "DELETE /admin/assignments/{assignment_id}",
    "DELETE /admin/fees",
    "DELETE /admin/settings",
    "GET /admin/admins",
    "GET /admin/alerts",
    "GET /admin/api-keys",
    "GET /admin/audit",
    "GET /admin/brands",
    "GET /admin/campaigns",
    "GET /admin/campaigns/{campaign_id}",
    "GET /admin/campaigns/{campaign_id}/vouchers",
    "GET /admin/chain-findings",
    "GET /admin/config-releases",
    "GET /admin/config/channels",
    "GET /admin/config/lease",
    "GET /admin/config/origins",
    "GET /admin/config/ota",
    "GET /admin/config/ota/placement",
    "GET /admin/config/qr",
    "GET /admin/config/retention",
    "GET /admin/config/scheduled",
    "GET /admin/config/tender",
    "GET /admin/config/vendors",
    "GET /admin/countries",
    "GET /admin/fees",
    "GET /admin/fees/effective",
    "GET /admin/fleet",
    "GET /admin/fleet/{store_id}",
    "GET /admin/health/tasks",
    "GET /admin/integrations/connections",
    "GET /admin/integrations/connections/{connection_id}",
    "GET /admin/integrations/providers",
    "GET /admin/invites",
    "GET /admin/locales",
    "GET /admin/openapi.json",
    "GET /admin/ota/releases/{release}",
    "GET /admin/ota/releases/{release}/installer",
    "GET /admin/reason-codes",
    "GET /admin/reason-codes/{reason_code_id}",
    "GET /admin/reconcile",
    "GET /admin/recovery-codes",
    "GET /admin/sessions",
    "GET /admin/settings",
    "GET /admin/settings/catalogue",
    "GET /admin/settings/effective",
    "GET /admin/stores",
    "GET /admin/stores/{store_id}/archives",
    "GET /admin/stores/{store_id}/config",
    "GET /admin/stores/{store_id}/config/versions/{version_id}",
    "GET /admin/stores/{store_id}/devices",
    "GET /admin/stores/{store_id}/devices/admitted",
    "GET /admin/subjects/{subject_id}",
    "GET /admin/subjects/{subject_id}/export",
    "GET /admin/tenants",
    "GET /admin/webhooks",
    "GET /admin/whoami",
    "PATCH /admin/admins/{id}/role",
    "PATCH /admin/admins/{id}/status",
    "PATCH /admin/brands/{brand_id}",
    "PATCH /admin/employees/{employee_id}",
    "PATCH /admin/roles/{role_id}",
    "PATCH /admin/stores/{store_id}",
    "PATCH /admin/stores/{store_id}/devices/{device_id}",
    "PATCH /admin/tenants/{tenant_id}",
    "POST /admin/api-keys",
    "POST /admin/assignments",
    "POST /admin/brands",
    "POST /admin/campaigns",
    "POST /admin/campaigns/{campaign_id}/vouchers",
    "POST /admin/config/campaigns/preview",
    "POST /admin/config/campaigns/schedule",
    "POST /admin/config/lease/bump",
    "POST /admin/config/lease/retire",
    "POST /admin/config/lease/settle",
    "POST /admin/config/ota/halt",
    "POST /admin/devices/publish",
    "POST /admin/employees",
    "POST /admin/fees",
    "POST /admin/fees/publish",
    "POST /admin/floor/areas",
    "POST /admin/floor/tables",
    "POST /admin/integrations/connections",
    "POST /admin/invites",
    "POST /admin/invites/accept",
    "POST /admin/kitchen/routing",
    "POST /admin/kitchen/stations",
    "POST /admin/login",
    "POST /admin/ota/releases",
    "POST /admin/ota/releases/fetch",
    "POST /admin/reason-codes",
    "POST /admin/recovery-codes",
    "POST /admin/roles",
    "POST /admin/settings/presets",
    "POST /admin/settings/publish",
    "POST /admin/setup",
    "POST /admin/stores",
    "POST /admin/stores/{store_id}/devices",
    "POST /admin/stores/{store_id}/region-acknowledgement",
    "POST /admin/subjects/{subject_id}/erase",
    "POST /admin/tenants",
    "POST /admin/totp",
    "POST /admin/webhooks",
    "PUT /admin/campaigns/{campaign_id}",
    "PUT /admin/config/campaigns",
    "PUT /admin/config/integrations",
    "PUT /admin/config/ota",
    "PUT /admin/config/ota/placement",
    "PUT /admin/config/reason-codes",
    "PUT /admin/fees",
    "PUT /admin/integrations/connections/{connection_id}",
    "PUT /admin/reason-codes/{reason_code_id}",
    "PUT /admin/settings",
    "PUT /admin/store-groups/{group_id}/members",
];

/// Store-box routes, `METHOD /path`, with a success answer or a request body that has no schema yet.
const DEVICE_NO_SCHEMA_YET: &[&str] = &[
    "GET /sync/stores/{store_id}/config",
    "GET /sync/stores/{store_id}/orders",
    "POST /claim/{claim_id}/collect",
    "POST /sync/stores/{store_id}/archive",
    "POST /sync/stores/{store_id}/archive-key",
    "POST /sync/stores/{store_id}/artifact",
    "POST /sync/stores/{store_id}/events",
    "POST /sync/stores/{store_id}/events/hello",
    "POST /sync/stores/{store_id}/heartbeat",
    "POST /sync/stores/{store_id}/orders/{queued_id}/ack",
    "POST /sync/stores/{store_id}/reconcile",
    "POST /sync/stores/{store_id}/report",
];

/// The operations in `document` with a success answer (other than `204`) that has no content, or
/// a request body with a media type that names no schema.
fn without_schema(document: &utoipa::openapi::OpenApi) -> BTreeSet<String> {
    let json = serde_json::to_value(document).expect("serialize the document");
    let mut missing = BTreeSet::new();
    for (path, item) in json["paths"].as_object().into_iter().flatten() {
        for (method, operation) in item.as_object().into_iter().flatten() {
            let route = format!("{} {path}", method.to_uppercase());
            let answers = operation["responses"].as_object().into_iter().flatten();
            let bare_answer = answers
                .filter(|(status, _)| status.starts_with('2') && *status != "204")
                .any(|(_, response)| response.get("content").is_none());
            let bare_request = operation.get("requestBody").is_some_and(|request| {
                let content = request["content"].as_object();
                content.is_none_or(|media| {
                    media.is_empty() || media.values().any(|one| one.get("schema").is_none())
                })
            });
            if bare_answer || bare_request {
                missing.insert(route);
            }
        }
    }
    missing
}

fn listed(routes: &[&str]) -> BTreeSet<String> {
    routes.iter().map(|&route| route.to_owned()).collect()
}

#[test]
fn every_admin_body_has_a_schema_or_is_listed() {
    assert_eq!(
        without_schema(&AdminApiDoc::openapi()),
        listed(ADMIN_NO_SCHEMA_YET),
        "a body without a schema must be in ADMIN_NO_SCHEMA_YET, and a route whose bodies all have one now \
         must leave it"
    );
}

#[test]
fn every_device_body_has_a_schema_or_is_listed() {
    assert_eq!(
        without_schema(&DeviceApiDoc::openapi()),
        listed(DEVICE_NO_SCHEMA_YET),
        "a body without a schema must be in DEVICE_NO_SCHEMA_YET, and a route whose bodies all have one now \
         must leave it"
    );
}

#[test]
fn every_v1_body_has_a_schema() {
    assert_eq!(without_schema(&ApiDoc::openapi()), BTreeSet::new());
}
