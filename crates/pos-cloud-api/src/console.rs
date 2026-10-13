// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a console route does around its work.

use std::sync::Arc;

use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;
use pos_proto::ulid::Ulid;

use crate::audit::{AuditActor, AuditEntry, AuditId, AuditRecorder};
use crate::auth::admin::{AdminContext, AdminStore, SessionDenied, authenticated_admin};
use crate::auth::console_rbac::{ConsolePermission, role_grants};
use crate::response::api_error;

/// The role-aware `/admin` guard ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)):
/// authenticates the session, resolves the acting admin, and checks that their role grants
/// `permission`. Every permission-gated `/admin` route calls this in place of the bare session check.
///
/// On success the acting [`AdminContext`] is returned (routes that only gate can ignore it). On
/// failure it returns the response to send: `401`/`503` from the session check (absent/invalid
/// session, or the store down), or a `403` when the session is valid but the role lacks the
/// permission — a distinct verdict, since the caller is authenticated and only under-privileged.
pub async fn require_permission<A, C>(
    admin_store: &A,
    clock: &C,
    headers: &HeaderMap,
    permission: ConsolePermission,
) -> Result<AdminContext, Response>
where
    A: AdminStore,
    C: ClockSource,
{
    let context = authenticated_admin(admin_store, clock, headers)
        .await
        .map_err(|denied: SessionDenied| denied.into_response())?;
    if role_grants(context.admin.role, permission) {
        Ok(context)
    } else {
        Err(api_error(
            ErrorStatus::PermissionDenied,
            "insufficient permissions",
        ))
    }
}

/// Records one console mutation to the audit trail ([ADR-0069](../../../docs/adr/0069-audit-trail.md)),
/// best-effort and after the mutation has already succeeded: mints the entry id from `clock`, snapshots
/// the acting admin from `actor`, and hands the entry to the recorder (which logs, never propagates, a
/// store failure). A mint failure is logged and the entry dropped rather than failing the caller's
/// completed mutation. `before` is the entity's prior value (`None` for a create), `after` its new
/// value (`None` for a delete); `tenant` scopes the entry (`None` for a tenant-global action).
#[expect(
    clippy::too_many_arguments,
    reason = "one audit entry is genuinely this many independent facts — recorder, clock, actor, \
              tenant scope, action, entity type+id, and the before/after values; bundling them into \
              a struct at every one of the many call sites would only add ceremony, not clarity"
)]
pub async fn audit_action<C>(
    audit: &Arc<dyn AuditRecorder>,
    clock: &C,
    actor: &AdminContext,
    tenant: Option<TenantId>,
    action: &str,
    entity_type: &str,
    entity_id: &str,
    before: Option<serde_json::Value>,
    after: Option<serde_json::Value>,
) where
    C: ClockSource,
{
    let now = clock.now();
    let Some(ulid) = mint_ulid(now.as_milliseconds_since_epoch()) else {
        tracing::error!(
            action,
            "could not mint an audit id; this action is not recorded"
        );
        return;
    };
    let entry = AuditEntry {
        id: AuditId::new(ulid),
        tenant_id: tenant,
        actor: AuditActor {
            admin_id: actor.admin.id.clone(),
            email: actor.admin.email.clone(),
            role: actor.admin.role,
        },
        action: action.to_owned(),
        entity_type: entity_type.to_owned(),
        entity_id: entity_id.to_owned(),
        before,
        after,
        request_id: None,
        at: now,
    };
    audit.record(entry).await;
}

/// The tenant a config request is scoped to. The super-admin is global, so it names the tenant on
/// the query string, exactly as API-key provisioning does.
#[derive(Debug, Clone, Deserialize)]
pub struct ConfigTenantQuery {
    /// The tenant whose store's config tree to author or read (a 26-character ULID).
    pub tenant_id: String,
}

/// A fresh ULID: `now_ms` as the timestamp and 80 CSPRNG bits as the randomness, or `None` if the OS
/// entropy source is unavailable (the caller then fails closed rather than mint a guessable id).
pub fn mint_ulid(now_ms: i64) -> Option<Ulid> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    let ms = u64::try_from(now_ms.max(0)).unwrap_or(0);
    // `Ulid::from_parts` masks the randomness to the low 80 bits the format defines.
    Some(Ulid::from_parts(ms, u128::from_le_bytes(bytes)))
}
