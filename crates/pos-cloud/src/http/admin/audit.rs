// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /admin/audit`: the console audit trail, windowed or paged, redacted by role (ADR-0069).

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::{api_error_with_details, service_unavailable, ulid_refusal};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;
use pos_proto::ulid::Ulid;
use serde::Deserialize;

use crate::audit::{AuditEntry, AuditStore, TrailOrder};
use crate::auth::admin::{AdminRole, AdminStore};
use crate::auth::console_rbac::{ConsolePermission, role_grants};
use crate::http::{
    Json, enum_refusal, offset_without_limit_refusal, page_bound_refusal, paged_ok, present_param,
};
use crate::paging::{Page, PageRequest, PageRequestError};

// --- Console audit read (`GET /admin/audit`, ADR-0069 slice 4) ----------------------------------

/// The collaborators the audit read needs, stated independently of [`CloudApp`]: the concrete audit
/// store (the [`AuditRecorder`] the write routes carry exposes only `record`, so the read carries the
/// store itself), plus the admin and clock the session guard uses.
#[derive(Clone)]
struct AuditReadState<Au, A, C> {
    audit: Au,
    admin: A,
    clock: C,
}

/// The default and maximum number of audit rows one read returns.
const AUDIT_READ_DEFAULT_LIMIT: u32 = 200;
const AUDIT_READ_MAX_LIMIT: u32 = 500;

/// The filters the Audit screen names on the query string. Every field is optional; an absent field
/// does not filter. `tenant_id` absent is the fleet-wide read (every tenant, including tenant-global
/// entries); present scopes to one tenant.
#[derive(Debug, Clone, Deserialize)]
/// `GET /admin/audit`: the filters, plus the bound.
///
/// `limit` is a `String` rather than a `u32` so that one parser owns it and an unparseable value
/// gets a refusal naming the field (ADR-0096's shape) instead of axum's own query rejection.
///
/// `offset` is what opts a caller into the paged form here, where every other paged read keys on
/// `limit` — see [`parse_audit_page`], which explains why this route could not use the same trigger.
///
/// `order` belongs to the paged form only, and the windowed read refuses it rather than ignoring it
/// — see [`windowed_read_cannot_be_ordered_refusal`].
struct AuditReadQuery {
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    entity_type: Option<String>,
    #[serde(default)]
    entity_id: Option<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    actor_admin_id: Option<String>,
    #[serde(default)]
    since_ms: Option<i64>,
    #[serde(default)]
    until_ms: Option<i64>,
    #[serde(default)]
    limit: Option<String>,
    #[serde(default)]
    offset: Option<String>,
    #[serde(default)]
    order: Option<String>,
}

/// One audit entry as the console reads it: ids as strings (the screen shows names/labels, not raw
/// ULIDs front-and-centre), the actor snapshot flattened, and the instant as Unix ms.
#[derive(Debug, Clone, serde::Serialize)]
pub(in crate::http) struct AuditEntryView {
    id: String,
    tenant_id: Option<String>,
    actor_admin_id: String,
    actor_email: String,
    actor_role: String,
    action: String,
    entity_type: String,
    entity_id: String,
    before: Option<serde_json::Value>,
    pub(in crate::http) after: Option<serde_json::Value>,
    request_id: Option<String>,
    at_ms: i64,
}

impl AuditEntryView {
    fn from_entry(entry: AuditEntry) -> Self {
        Self {
            id: entry.id.to_string(),
            tenant_id: entry.tenant_id.map(|tenant| tenant.to_string()),
            actor_admin_id: entry.actor.admin_id,
            actor_email: entry.actor.email,
            actor_role: entry.actor.role.as_token().to_owned(),
            action: entry.action,
            entity_type: entry.entity_type,
            entity_id: entry.entity_id,
            before: entry.before,
            after: entry.after,
            request_id: entry.request_id,
            at_ms: entry.at.as_milliseconds_since_epoch(),
        }
    }
}

/// Builds the audit-read sub-router ([ADR-0069](../../../docs/adr/0069-audit-trail.md) slice 4).
///
/// One read, `GET /admin/audit`, behind [`ConsolePermission::Read`] (every console role may read the
/// trail). An entry about an employee records the person's staff code, so a role without
/// [`ConsolePermission::ReadPeople`] reads that entry without it
/// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md) decision 9).
/// It names its tenant the admin-is-global way — a `?tenant_id=` query
/// ([ADR-0060](../../../docs/adr/0060-cloud-back-office-dashboard.md)); absent, it is the fleet-wide
/// read. Like the other reads it carries its own state and is merged into the main router.
pub fn audit_router<Au, A, C>(audit: Au, admin: A, clock: C) -> Router
where
    Au: AuditStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/audit", get(admin_list_audit::<Au, A, C>))
        .with_state(AuditReadState {
            audit,
            admin,
            clock,
        })
}

#[utoipa::path(
    get,
    path = "/admin/audit",
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = Option<String>, Query, description = "One tenant's entries (a 26-character \
                                                           ULID). Absent, the fleet-wide trail, \
                                                           tenant-global entries included"),
        ("entity_type" = Option<String>, Query, description = "Only entries about this kind of \
                                                             record, such as `store`"),
        ("entity_id" = Option<String>, Query, description = "Only entries about this record"),
        ("action" = Option<String>, Query, description = "Only entries with this action, such as \
                                                        `store.update`"),
        ("actor_admin_id" = Option<String>, Query, description = "Only entries by this administrator"),
        ("since_ms" = Option<i64>, Query, description = "Only entries at or after this instant (Unix ms)"),
        ("until_ms" = Option<i64>, Query, description = "Only entries at or before this instant (Unix ms)"),
        ("limit" = Option<String>, Query, description = "Without `offset`, the most recent this many \
                                                       entries (default 200, larger values read as \
                                                       500). With `offset`, the page size, 1 to 500, \
                                                       refused rather than clamped"),
        ("offset" = Option<String>, Query, description = "Asks for a page: how many entries to skip, \
                                                        0 to 100000"),
        ("order" = Option<String>, Query, description = "`newest` (the default) or `oldest`. Only \
                                                       with `offset`"),
    ),
    responses(
        (status = 200, description = "Without `offset`, the newest matching entries as an array; \
                                      with it, a page: `items`, `total`, `limit` and `offset`. Each \
                                      entry has `id`, `tenant_id`, the acting administrator's id, \
                                      email and role, `action`, `entity_type`, `entity_id`, \
                                      `before`, `after`, `request_id` and `at_ms`. An entry about an \
                                      employee carries the person's staff code only for a role \
                                      holding console.people.read (ADR-0158 decision 9)"),
        (status = 400, description = "tenant_id is not a ULID; limit or offset is not an integer \
                                      in range; or order is sent without offset or is not one the \
                                      route accepts", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The audit store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "audit",
)]
/// A super-admin (any console role) reads the audit trail, filtered by the query string.
async fn admin_list_audit<Au, A, C>(
    State(state): State<AuditReadState<Au, A, C>>,
    headers: HeaderMap,
    Query(query): Query<AuditReadQuery>,
) -> Response
where
    Au: AuditStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let role = context.admin.role;
    let tenant = match query.tenant_id.as_deref() {
        Some(text) => match text.parse::<Ulid>().map(TenantId::new) {
            Ok(tenant) => Some(tenant),
            Err(_ignored) => {
                return ulid_refusal(&["tenant_id"]);
            }
        },
        None => None,
    };
    let filter = crate::audit::AuditQuery {
        tenant,
        entity_type: query.entity_type,
        entity_id: query.entity_id,
        action: query.action,
        actor_admin_id: query.actor_admin_id,
        since_ms: query.since_ms,
        until_ms: query.until_ms,
    };
    // Two reads over the same filters, chosen by whether the caller named an offset (ADR-0098). The
    // per-entity audit panel wants the newest few for one entity and no count; the Audit screen's
    // table wants a page and a total.
    let Some(page) = parse_audit_page(query.limit.as_deref(), query.offset.as_deref()) else {
        if present_param(query.order.as_deref()).is_some() {
            return windowed_read_cannot_be_ordered_refusal();
        }
        let limit = match windowed_audit_limit(query.limit.as_deref()) {
            Ok(limit) => limit,
            Err(refusal) => return refusal,
        };
        return match state.audit.query(&filter, limit).await {
            Ok(entries) => (StatusCode::OK, Json(audit_views(entries, role))).into_response(),
            Err(error) => audit_read_failure(&error),
        };
    };
    let page = match page {
        Ok(page) => page,
        Err(refusal) => return refusal,
    };
    let order = match parse_trail_order(query.order.as_deref()) {
        Ok(order) => order,
        Err(refusal) => return refusal,
    };
    match state.audit.query_page(&filter, page, order).await {
        Ok(read) => paged_ok(Page::new(audit_views(read.items, role), read.total), page),
        Err(error) => audit_read_failure(&error),
    }
}

/// Reads `?order=` off the paged audit read, defaulting to the trail's own order.
///
/// # Errors
///
/// A closed-set refusal naming `order` and the tokens it accepts, if the value names no order.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the refusal is built where the field names are \
              known, exactly as the other query parsers on this router do"
)]
fn parse_trail_order(order: Option<&str>) -> Result<TrailOrder, Response> {
    match present_param(order) {
        None => Ok(TrailOrder::default()),
        Some(token) => TrailOrder::from_token(token)
            .ok_or_else(|| enum_refusal("order", TrailOrder::tokens().iter().copied())),
    }
}

/// The refusal for `?order=` sent to the *windowed* audit read.
///
/// Its own sentence rather than [`page_shaping_needs_a_limit_refusal`]'s, because on this route the
/// missing parameter is the `offset` and the reason is sharper than "it would be ignored": on the
/// windowed read `limit` already means "the most recent this many", so `?order=oldest&limit=200`
/// has two honest readings — the newest two hundred entries shown earliest-first, or the earliest
/// two hundred entries — and they are different sets. The route will not guess between them.
///
/// The paged read has no such ambiguity, which is why the order lives there: `LIMIT`/`OFFSET`
/// window an already-ordered set, so the order is applied first and the window second, always.
fn windowed_read_cannot_be_ordered_refusal() -> Response {
    api_error_with_details(
        ErrorStatus::InvalidArgument,
        "order shapes a page and needs an offset: on this read `limit` means \"the most recent this \
         many\", so an order would have two readings — the newest entries earliest-first, or the \
         earliest entries — and those are different sets",
        &[("order", "MISSING_DEPENDENT_FIELD")],
    )
}

/// The wire view of a batch of audit entries, shared by the windowed and paged reads, as `role`
/// may read them: without the staff code of an employee entry unless it holds
/// [`ConsolePermission::ReadPeople`] ([`without_audited_staff_identity`]).
pub(in crate::http) fn audit_views(
    entries: Vec<AuditEntry>,
    role: AdminRole,
) -> Vec<AuditEntryView> {
    let names_staff = role_grants(role, ConsolePermission::ReadPeople);
    entries
        .into_iter()
        .map(|entry| {
            if names_staff {
                entry
            } else {
                without_audited_staff_identity(entry)
            }
        })
        .map(AuditEntryView::from_entry)
        .collect()
}

/// Removes the staff code, and a name if one is there, from the `before` and `after` of an audit
/// entry about an employee, for a reader without [`ConsolePermission::ReadPeople`]
/// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md) decision 9).
///
/// `employee.create` and `employee.update` record the person's id, staff code and status
/// (ADR-0070), and the trail is behind [`ConsolePermission::Read`], which every console role holds.
/// Without this the Audit screen would be the way round the roster's gate, as the config screen
/// was before [`without_staff_identities`]: a Viewer refused the roster could collect every staff
/// code the console ever wrote, each beside the employee id it belongs to.
///
/// **Only `employee` entries**, because only they identify a person. An `assignment` records ids,
/// a `role`'s `name` is the role's, and a `store`'s is the shop's. The `name` goes as well as the
/// `code` although no route writes one, because ADR-0070 forbids it and this read should not rely
/// on every writer, past and future, remembering that. Everything else stays, so the trail still
/// says who changed which employee, how and when. The fields are *removed*, not blanked, for the
/// reason S7 gives. What the trail *records* is unchanged: an Owner or Admin reads the same entry
/// with the code in it.
pub(in crate::http) fn without_audited_staff_identity(mut entry: AuditEntry) -> AuditEntry {
    if entry.entity_type == "employee" {
        for recorded in [&mut entry.before, &mut entry.after].into_iter().flatten() {
            if let Some(fields) = recorded.as_object_mut() {
                fields.remove("code");
                fields.remove("name");
            }
        }
    }
    entry
}

/// Maps an audit read failure to a retryable `503`, logging the detail rather than leaking it.
fn audit_read_failure(error: &crate::audit::AuditStoreError) -> Response {
    tracing::error!(%error, "an audit read failed");
    service_unavailable("audit")
}

/// The bound for the *windowed* audit read: how many of the newest matching entries to return.
///
/// Defaulted and clamped, which is ADR-0069's behaviour for this read and is deliberately kept.
/// `limit` here does not mean "one page of this size" — it means "the most recent this many" — so
/// pulling an over-large value into range answers the question the caller asked, just less of it.
/// That is the opposite of the paged form, where a clamp would answer a *different* question (see
/// [`crate::paging`]).
///
/// A value that is not a number is still refused: there is no honest reading of `?limit=lots`.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the refusal is built where the field names are \
              known, exactly as the other query parsers on this router do"
)]
fn windowed_audit_limit(limit: Option<&str>) -> Result<u32, Response> {
    match present_param(limit) {
        None => Ok(AUDIT_READ_DEFAULT_LIMIT),
        Some(text) => match text.parse::<u32>() {
            Ok(value) => Ok(value.clamp(1, AUDIT_READ_MAX_LIMIT)),
            Err(_ignored) => Err(page_bound_refusal("limit")),
        },
    }
}

/// Reads the paging bounds off `/admin/audit`, where the trigger is `offset` and not `limit`.
///
/// `None` means "not a paged request" and the caller should serve the windowed read.
///
/// # Why this route's trigger differs
///
/// Everywhere else, naming a `limit` is what asks for a page, because `limit` did not previously
/// exist on those reads. On `/admin/audit` it did: ADR-0069 gave it a *different* meaning — "the most
/// recent this many", defaulted to 200 and clamped at 500 — and the console sends it today. Making
/// `?limit=200` return a paged envelope instead of a bare array would change the response shape of a
/// request already in flight, which is the one thing ADR-0098 exists to prevent. So on this route the
/// second read is asked for by naming an `offset`.
///
/// ADR-0098 put `audit` in the paged cohort without drawing this consequence from its own
/// measurement that five query structs already carry a `limit`. That is recorded as a correction in
/// the ADR rather than left as a surprise here.
///
/// Within the paged form the `limit` is *checked*, not clamped, exactly as elsewhere: it is a page
/// size, and a caller stitching pages together must be told when the size it used was not the size
/// it asked for.
fn parse_audit_page(
    limit: Option<&str>,
    offset: Option<&str>,
) -> Option<Result<PageRequest, Response>> {
    let offset_text = present_param(offset)?;
    let Some(limit_text) = present_param(limit) else {
        // A page is a limit and an offset together. Defaulting the limit here would make the page
        // size invisible to the caller stitching pages, which decision 1 rules out.
        return Some(Err(offset_without_limit_refusal()));
    };
    let Ok(limit) = limit_text.parse::<u32>() else {
        return Some(Err(page_bound_refusal("limit")));
    };
    let Ok(offset) = offset_text.parse::<u32>() else {
        return Some(Err(page_bound_refusal("offset")));
    };
    Some(match PageRequest::new(limit, offset) {
        Ok(request) => Ok(request),
        Err(PageRequestError::LimitOutOfRange) => Err(page_bound_refusal("limit")),
        Err(PageRequestError::OffsetOutOfRange) => Err(page_bound_refusal("offset")),
    })
}
