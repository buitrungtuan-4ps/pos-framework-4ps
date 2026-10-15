// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Reason codes: the codes a tenant authors under `/admin/reason-codes`, and their publish to a
//! store's `reason_codes` node (ADR-0115).
//!
//! The state, the request body and its audit summary, the router and the error helpers are here.

use std::sync::Arc;

use authoring::{
    admin_create_reason_code, admin_delete_reason_code, admin_get_reason_code,
    admin_list_reason_codes, admin_update_reason_code,
};
use axum::Router;
use axum::response::Response;
use axum::routing::get;
use pos_cloud_api::response::api_error;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::reason_codes::{PublishedReasonCode, ReasonAction};
use pos_proto::wire_enum::Open;
use serde::Deserialize;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::reason_codes::{ReasonCodeStore, ReasonCodeStoreError};

mod authoring;
mod publish;

// The handlers the console's OpenAPI document names.
pub(crate) use authoring::{
    __path_admin_create_reason_code, __path_admin_delete_reason_code, __path_admin_get_reason_code,
    __path_admin_list_reason_codes, __path_admin_update_reason_code,
};
pub use publish::config_reason_codes_router;
// What the batch fan-out uses.
pub(crate) use publish::__path_admin_publish_reason_codes;
pub(super) use publish::reason_code_nodes;

// --- Reason-code authoring (`/admin/reason-codes/*`, ADR-0115, roadmap B2.2) -------------------

/// The state the reason-code routes share: the store the entries are read and written on, and the
/// admin/clock/audit every `/admin` write needs.
#[derive(Clone)]
struct ReasonCodeState<R, A, C> {
    reason_codes: R,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A create/update body for a reason code. The id is **server-owned** — minted on create, the path
/// id on update — so a client never supplies or forges the value historic events cite forever.
///
/// `code`, `display_name`, `display_name_translations`, `applies_to` and `active` are the wire
/// entry's own fields, reused rather than re-declared, except that the two name fields arrive as
/// plain strings so a blank a form left behind can be trimmed away before it becomes a
/// [`DisplayName`].
#[derive(Debug, Clone, Deserialize)]
struct ReasonCodeRequest {
    tenant_id: String,
    code: String,
    display_name: String,
    /// Per-locale names keyed by locale code (`docs/pos-spec.md` §12). Optional and additive;
    /// `display_name` is the always-present fallback.
    #[serde(default)]
    display_name_translations: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    applies_to: Vec<Open<ReasonAction>>,
    /// Whether staff may still pick it. Absent means active, matching the wire node's own default —
    /// a caller that omits it is authoring a usable reason, not a retired one.
    #[serde(default = "authored_reason_is_active")]
    active: bool,
}

/// The default for [`ReasonCodeRequest::active`]: an authored reason is one staff may pick.
const fn authored_reason_is_active() -> bool {
    true
}

/// A compact audit summary of a reason code — reference data throughout (id, code, name, the
/// actions it covers, whether it is live), never a T1 field.
///
/// `applies_to` and `active` are in the summary rather than left to the record, because this is a
/// **fraud control** (`docs/pos-spec.md` §11 item 2) and the two ways to defeat one are to narrow
/// the actions a reason covers and to retire the reason a colleague's void would have had to cite.
/// A summary that omitted them would record *that* a reason changed while hiding the change that
/// mattered.
#[derive(Debug, Clone, serde::Serialize)]
struct ReasonCodeAuditSummary {
    reason_code_id: String,
    code: String,
    name: String,
    applies_to: Vec<String>,
    active: bool,
}

impl ReasonCodeAuditSummary {
    fn of(reason_code: &PublishedReasonCode) -> Self {
        Self {
            reason_code_id: reason_code.id.to_string(),
            code: reason_code.code.as_str().to_owned(),
            name: reason_code.display_name.as_str().to_owned(),
            applies_to: reason_code
                .applies_to
                .iter()
                .map(|action| action.as_wire().to_owned())
                .collect(),
            active: reason_code.active,
        }
    }
}

/// Builds the reason-code sub-router
/// ([ADR-0115](../../../../docs/adr/0115-reason-codes-are-a-managed-list.md), B2.2 slice 3).
///
/// Per-record CRUD over a tenant's managed reason codes — the list a void, a discount, a comp, a
/// refund, a drawer opening, a staff rejection, either cash movement and either stock correction
/// must cite. `GET` (list and by-id) is behind [`ConsolePermission::Read`](crate::auth::console_rbac::ConsolePermission::Read); the writes are behind
/// [`ConsolePermission::ManageReasonCodes`](crate::auth::console_rbac::ConsolePermission::ManageReasonCodes), because authoring the list is what defeats or upholds
/// the control and is not the same act as publishing it. Ids are server-minted: an event names a
/// [`ReasonCodeId`](pos_proto::ids::ReasonCodeId) forever, so a client that could choose one could collide with an id already in
/// the log.
///
/// `DELETE` is here for the entry created by mistake and never cited. The way an entry leaves
/// service is `active: false` through the ordinary `PUT` — see [`crate::reason_codes`] — and the
/// console leads with that, putting delete behind a typed-name confirmation. Composing the authored
/// entries into the published `reason_codes` node is a separate route (a later slice).
pub fn reason_code_router<R, A, C>(
    reason_codes: R,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    R: ReasonCodeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/reason-codes",
            get(admin_list_reason_codes::<R, A, C>).post(admin_create_reason_code::<R, A, C>),
        )
        .route(
            "/admin/reason-codes/{reason_code_id}",
            get(admin_get_reason_code::<R, A, C>)
                .put(admin_update_reason_code::<R, A, C>)
                .delete(admin_delete_reason_code::<R, A, C>),
        )
        .with_state(ReasonCodeState {
            reason_codes,
            admin,
            clock,
            audit,
        })
}

/// The `503` a reason-code create earns when the OS will not give the entropy its id needs.
fn reason_code_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy to mint a reason code id");
    api_error(
        ErrorStatus::Unavailable,
        "the reason code service is unavailable",
    )
}

/// Maps a reason-code store failure to a retryable `503`, logging the detail rather than leaking it.
fn reason_code_error_response(error: &ReasonCodeStoreError) -> Response {
    tracing::error!(%error, "a reason code store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the reason code service is unavailable",
    )
}
