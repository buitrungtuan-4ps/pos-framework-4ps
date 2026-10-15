// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The translation grid (`/admin/translations`): read, write, export and import a tenant's
//! translations.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{
    api_error, api_error_with_details, csv_download_response, parse_ulid_fields,
};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;
use serde::Deserialize;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::config_publish::CONDITIONAL_WRITE_ATTEMPTS;
use crate::http::{Json, if_match_collection, not_found, version_mismatch, with_etag};
use crate::translations::{TranslationGrid, TranslationStore};
use crate::version::UpdateOutcome;
use crate::{export, import};

// --- Translation grid (`/admin/translations`) ---------------------------------------------------

/// The collaborators the translation-grid routes need, stated independently of [`CloudApp`].
#[derive(Clone)]
struct TranslationState<Tr, A, C> {
    translations: Tr,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// The tenant a translation request is scoped to (the super-admin is global).
#[derive(Debug, Clone, Deserialize)]
struct TranslationTenantQuery {
    /// The tenant whose grid to read or write (a 26-character ULID).
    tenant_id: String,
}

/// Builds the translation-grid sub-router, stated independently of [`CloudApp`]
/// ([ADR-0043](../../../../docs/adr/0043-translation-grid.md)).
///
/// A super-admin reads and replaces a tenant's whole grid behind the session guard; a `PUT` is
/// validated so every key carries a non-empty `en` fallback before anything is stored. Like the other
/// merged sub-routers, it carries its own state rather than adding a `CloudApp` generic.
pub fn translation_router<Tr, A, C>(
    translations: Tr,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Tr: TranslationStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/translations",
            get(get_translations::<Tr, A, C>).put(put_translations::<Tr, A, C>),
        )
        .route(
            "/admin/translations/export",
            get(export_translations::<Tr, A, C>),
        )
        .route(
            "/admin/translations/import/dry-run",
            post(import_translations_dry_run::<Tr, A, C>),
        )
        .route(
            "/admin/translations/import/apply",
            post(import_translations_apply::<Tr, A, C>),
        )
        .layer(axum::extract::DefaultBodyLimit::max(MAX_CSV_IMPORT_BYTES))
        .with_state(TranslationState {
            translations,
            admin,
            clock,
            audit,
        })
}

/// Returns a tenant's translation grid (super-admin only), or an empty grid if it has authored none.
async fn get_translations<Tr, A, C>(
    State(state): State<TranslationState<Tr, A, C>>,
    headers: HeaderMap,
    Query(query): Query<TranslationTenantQuery>,
) -> Response
where
    Tr: TranslationStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.translations.load(tenant_id).await {
        // A tenant with no grid yet is an empty grid to edit, not a 404 — and no `ETag`, because
        // there is no version yet. That absence is what the console turns into `If-Match: *`.
        Ok(None) => (StatusCode::OK, Json(TranslationGrid::default())).into_response(),
        Ok(Some(loaded)) => {
            let response = (StatusCode::OK, Json(loaded.record)).into_response();
            with_etag(response, &loaded.etag)
        }
        Err(error) => translation_error_response(&error),
    }
}

/// Replaces a tenant's translation grid (super-admin only). A grid whose every key does not carry a
/// non-empty `en` is a `422` naming the offending keys, and nothing is stored.
async fn put_translations<Tr, A, C>(
    State(state): State<TranslationState<Tr, A, C>>,
    headers: HeaderMap,
    Query(query): Query<TranslationTenantQuery>,
    Json(grid): Json<TranslationGrid>,
) -> Response
where
    Tr: TranslationStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageTranslations,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    // The version the console read the grid at, or `None` from `If-Match: *` for a tenant that has
    // authored no grid yet ([ADR-0095](../../../../docs/adr/0095-conditional-writes-for-collections.md)).
    // This route replaces a grid an operator edited cell by cell, so a second writer really does
    // destroy their work — unlike the CSV import below, which merges and can retry.
    let expected = match if_match_collection(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    let missing = grid.keys_missing_fallback();
    if !missing.is_empty() {
        // The one refusal in this file that knows exactly which fields are at fault, so it is the
        // one that most needs `details` — and the one that had the worst behaviour before ADR-0096:
        // its bespoke `{"missing_fallback":[…]}` body had no reader in the console at all, so an
        // operator's toast rendered the raw JSON.
        //
        // `<key>.en` rather than `<key>`: it is the `en` value that is missing, and a path a reader
        // can act on beats a name they have to interpret.
        let fields: Vec<String> = missing.iter().map(|key| format!("{key}.en")).collect();
        let details: Vec<(&str, &str)> = fields
            .iter()
            .map(|field| (field.as_str(), "REQUIRED"))
            .collect();
        return api_error_with_details(
            ErrorStatus::Unprocessable,
            format!(
                "every key needs a non-empty `en` fallback; these do not: {}",
                missing.join(", ")
            ),
            &details,
        );
    }
    match state
        .translations
        .save(tenant_id, &grid, expected.as_ref())
        .await
    {
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        // No grid to replace, and the caller named a version for one: an absence, not a conflict.
        Ok(UpdateOutcome::NotFound) => not_found("translation grid"),
        Ok(UpdateOutcome::Updated(version)) => {
            // A whole-grid replace: the entity is the tenant's grid, keyed by the tenant id. The grid
            // is business copy (menu/UI strings), not personal data, so it is recorded as `after`.
            let after = serde_json::to_value(&grid).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "translations.save",
                "translation_grid",
                &tenant_id.to_string(),
                None,
                after,
            )
            .await;
            with_etag(StatusCode::NO_CONTENT.into_response(), &version)
        }
        Err(error) => translation_error_response(&error),
    }
}

/// A super-admin exports a tenant's translation grid as CSV ([ADR-0075](../../../../docs/adr/0075-media-and-file-rail.md),
/// Track M5). Behind [`ConsolePermission::ManageTranslations`] and audited (who exported which domain
/// and how many rows, never the row contents). The grid is business copy (menu/UI strings), not
/// personal data.
async fn export_translations<Tr, A, C>(
    State(state): State<TranslationState<Tr, A, C>>,
    headers: HeaderMap,
    Query(query): Query<TranslationTenantQuery>,
) -> Response
where
    Tr: TranslationStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageTranslations,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let grid = match state.translations.load(tenant_id).await {
        Ok(grid) => grid.map(|loaded| loaded.record).unwrap_or_default(),
        Err(error) => return translation_error_response(&error),
    };
    let Ok(body) = export::translations_csv(&grid) else {
        return api_error(ErrorStatus::Internal, "could not build the CSV");
    };
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "translations.export",
        "translation_export",
        &tenant_id.to_string(),
        None,
        Some(serde_json::json!({ "domain": "translations", "rows": grid.as_map().len() })),
    )
    .await;
    csv_download_response("translations.csv", body)
}

/// The hard cap on a CSV import body ([ADR-0075](../../../../docs/adr/0075-media-and-file-rail.md)): a
/// translation grid is small copy, so 4 MB is generous while bounding the parse.
const MAX_CSV_IMPORT_BYTES: usize = 4 * 1024 * 1024;

/// A super-admin dry-runs a translation-grid CSV import ([ADR-0075](../../../../docs/adr/0075-media-and-file-rail.md),
/// Track M5): the server parses and classifies every row (would-create / would-update / rejected) and
/// returns the report, **writing nothing**. Behind [`ConsolePermission::ManageTranslations`]; not
/// audited (it changes nothing). The confirm step (`.../apply`) does the write.
async fn import_translations_dry_run<Tr, A, C>(
    State(state): State<TranslationState<Tr, A, C>>,
    headers: HeaderMap,
    Query(query): Query<TranslationTenantQuery>,
    body: axum::body::Bytes,
) -> Response
where
    Tr: TranslationStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageTranslations,
    )
    .await
    {
        return denied;
    }
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let existing = match state.translations.load(tenant_id).await {
        Ok(grid) => grid.map(|loaded| loaded.record).unwrap_or_default(),
        Err(error) => return translation_error_response(&error),
    };
    match import::parse_translations_csv(&body, &existing) {
        Ok((_, report)) => (StatusCode::OK, Json(report)).into_response(),
        // No `details`: the parser addresses a row and byte offset in an opaque uploaded body, and
        // the only named input this request has is the `tenant_id` query, which is not what failed.
        // Inventing a field here would send a client looking for a JSON member it never sent.
        Err(error) => api_error(ErrorStatus::InvalidArgument, error.to_string()),
    }
}

/// A super-admin applies a translation-grid CSV import ([ADR-0075](../../../../docs/adr/0075-media-and-file-rail.md),
/// Track M5) — the confirm after a dry-run. The valid rows are merged onto the tenant's grid (existing
/// keys not in the file are preserved; rejected rows are skipped) and saved; rejected rows are reported,
/// never written. Behind [`ConsolePermission::ManageTranslations`] and audited (the row counts, never
/// the contents). The merged grid still satisfies the `en`-fallback rule by construction.
async fn import_translations_apply<Tr, A, C>(
    State(state): State<TranslationState<Tr, A, C>>,
    headers: HeaderMap,
    Query(query): Query<TranslationTenantQuery>,
    body: axum::body::Bytes,
) -> Response
where
    Tr: TranslationStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageTranslations,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    // An import **merges** its rows into whatever grid is stored: the keys in the CSV are set, every
    // other key is left alone. That makes it a delta, not a replace, so losing a race to another
    // writer is not a lost update — it is a stale read, and re-reading fixes it. Retrying is the
    // right answer where refusing would be for `PUT /admin/translations`, because there is no
    // operator holding a version to be told about, and the rows to apply are in the request
    // ([ADR-0095](../../../../docs/adr/0095-conditional-writes-for-collections.md)).
    let mut applied = None;
    for _ in 0..CONDITIONAL_WRITE_ATTEMPTS {
        let loaded = match state.translations.load(tenant_id).await {
            Ok(loaded) => loaded,
            Err(error) => return translation_error_response(&error),
        };
        let (existing, expected) = loaded.map_or_else(
            || (TranslationGrid::default(), None),
            |loaded| (loaded.record, Some(loaded.etag)),
        );
        // Re-parsed against the grid this attempt actually read, so the report counts creates and
        // updates relative to the winner's grid rather than to one that no longer exists.
        let (merged, report) = match import::parse_translations_csv(&body, &existing) {
            Ok(parsed) => parsed,
            Err(error) => return api_error(ErrorStatus::InvalidArgument, error.to_string()),
        };
        match state
            .translations
            .save(tenant_id, &merged, expected.as_ref())
            .await
        {
            Ok(UpdateOutcome::Updated(version)) => {
                applied = Some((report, version));
                break;
            }
            Ok(UpdateOutcome::VersionMismatch | UpdateOutcome::NotFound) => {}
            Err(error) => return translation_error_response(&error),
        }
    }
    let Some((report, version)) = applied else {
        return version_mismatch();
    };
    {
        {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "translations.import",
                "translation_grid",
                &tenant_id.to_string(),
                None,
                Some(serde_json::json!({
                    "created": report.create_count,
                    "updated": report.update_count,
                    "rejected": report.reject_count,
                })),
            )
            .await;
            with_etag((StatusCode::OK, Json(report)).into_response(), &version)
        }
    }
}

/// Maps a translation-store failure to a retryable `503`, logging the detail rather than leaking it.
fn translation_error_response(error: &crate::translations::TranslationStoreError) -> Response {
    tracing::error!(%error, "a translation store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the translation service is unavailable",
    )
}
