// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The translation grid (`/admin/translations`): the round trip and the en fallback, CSV export and
//! import, and the session every route needs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud::translations::{TranslationGrid, TranslationStore, TranslationStoreError};
use pos_cloud::version::{UpdateOutcome, Version, Versioned};
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::ids::TenantId;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeConfigTrees, FakeKeys, FakeRollups, FakeWebhooks, admin_cookie, app_all, clock,
    etag_of, get, get_with_cookie, json_body, post_bytes_with_cookie, provisioned_admin,
    put_with_etag, tenant, text_body,
};

// --- Translation grid (`/admin/translations`, behind the session guard) -------------------------

/// The translation store, one grid per tenant, each carrying the version it was last written at —
/// the `grid` and `xmin` of one `translations` row ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)).
#[derive(Clone, Default)]
struct FakeTranslations {
    rows: Arc<Mutex<HashMap<TenantId, Versioned<TranslationGrid>>>>,
    next_version: Arc<Mutex<u64>>,
    /// A competing save to land *between* the next read and its write, so a test can produce the
    /// race the CSV import's retry exists for. `(key, locale, value)` is merged into the grid.
    interpose: Arc<Mutex<Option<(String, String, String)>>>,
}

impl FakeTranslations {
    fn mint(&self) -> Version {
        let mut next = self.next_version.lock().expect("lock");
        *next += 1;
        Version::new(format!("tg{next}"))
    }
}

impl TranslationStore for FakeTranslations {
    async fn load(
        &self,
        tenant: TenantId,
    ) -> Result<Option<Versioned<TranslationGrid>>, TranslationStoreError> {
        let handed_out = self.rows.lock().expect("lock").get(&tenant).cloned();
        // The competing save lands after this read has been taken but before the caller can write,
        // which is exactly the interleave a retry has to survive.
        if let Some((key, locale, value)) = self.interpose.lock().expect("lock").take() {
            let version = self.mint();
            let mut rows = self.rows.lock().expect("lock");
            let mut entries = rows
                .get(&tenant)
                .map(|loaded| loaded.record.as_map().clone())
                .unwrap_or_default();
            entries.entry(key).or_default().insert(locale, value);
            rows.insert(
                tenant,
                Versioned::new(TranslationGrid::new(entries), version),
            );
        }
        Ok(handed_out)
    }

    async fn save(
        &self,
        tenant: TenantId,
        grid: &TranslationGrid,
        expected: Option<&Version>,
    ) -> Result<UpdateOutcome, TranslationStoreError> {
        let version = self.mint();
        let mut rows = self.rows.lock().expect("lock");
        // The same four answers `store-postgres` gives, so a test passing here is not passing on a
        // laxer store.
        let refusal = match (rows.get(&tenant), expected) {
            (None, None) => None,
            (None, Some(_)) => Some(UpdateOutcome::NotFound),
            (Some(_), None) => Some(UpdateOutcome::VersionMismatch),
            (Some(stored), Some(expected)) => {
                (&stored.etag != expected).then_some(UpdateOutcome::VersionMismatch)
            }
        };
        if let Some(refusal) = refusal {
            return Ok(refusal);
        }
        rows.insert(tenant, Versioned::new(grid.clone(), version.clone()));
        Ok(UpdateOutcome::Updated(version))
    }
}

/// The main router (for `/admin/login`) and the translation sub-router, sharing one admin store.
fn translation_app(admin: FakeAdmin, translations: FakeTranslations) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::translation_router(
        translations,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

#[tokio::test]
async fn translation_grid_round_trips_and_enforces_the_en_fallback() {
    let router = translation_app(provisioned_admin(), FakeTranslations::default());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let uri = format!("/admin/translations?tenant_id={tenant_ulid}");

    // A grid with en on every key publishes and round-trips through GET. `If-Match: *` is how a
    // tenant that has authored nothing yet says so (ADR-0095).
    let good = serde_json::json!({
        "menu.pho": { "en": "Pho", "vi": "Phở" },
        "menu.tea": { "en": "Tea" },
    });
    let put = router
        .clone()
        .oneshot(put_with_etag(&uri, &good, &cookie, "*"))
        .await
        .expect("route the publish");
    assert_eq!(put.status(), StatusCode::NO_CONTENT);
    let got = router
        .clone()
        .oneshot(get_with_cookie(&uri, &cookie))
        .await
        .expect("route the read");
    assert_eq!(got.status(), StatusCode::OK);
    let current = etag_of(&got);
    assert_eq!(json_body(got).await, good, "the grid round-trips");

    // A grid missing en on a key is a 422 naming it, and does not overwrite the good grid.
    let bad = serde_json::json!({ "menu.rice": { "vi": "Cơm" } });
    let rejected = router
        .clone()
        .oneshot(put_with_etag(&uri, &bad, &cookie, &current))
        .await
        .expect("route the bad publish");
    assert_eq!(rejected.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let refusal = json_body(rejected).await;
    assert_eq!(refusal["error"]["status"], "UNPROCESSABLE", "got {refusal}");
    // The whole point of ADR-0096 for this route: the offending key reaches the console as a field
    // it can mark, not as a raw `{"missing_fallback":[…]}` body nothing there reads.
    assert_eq!(
        refusal["error"]["details"][0]["field"], "menu.rice.en",
        "the rejection names the key lacking an en fallback: {refusal}"
    );
    assert_eq!(refusal["error"]["details"][0]["reason"], "REQUIRED");
    assert!(
        refusal["error"]["details"][1].is_null(),
        "one detail per offending key, and only one key is bad: {refusal}"
    );
    assert!(
        refusal["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("menu.rice")),
        "and the sentence a person reads names it too: {refusal}"
    );
    let unchanged = router
        .clone()
        .oneshot(get_with_cookie(&uri, &cookie))
        .await
        .expect("route the re-read");
    assert_eq!(
        json_body(unchanged).await,
        good,
        "a rejected publish left the last good grid current"
    );

    // Replaying `*` after the grid exists is the claim "nothing is saved here", and it is false.
    let stale_wildcard = router
        .clone()
        .oneshot(put_with_etag(&uri, &good, &cookie, "*"))
        .await
        .expect("route the stale publish");
    assert_eq!(
        stale_wildcard.status(),
        StatusCode::PRECONDITION_FAILED,
        "a wildcard is an assertion about an empty grid, not a waiver"
    );

    // A save at the current version applies and moves it; replaying that version is then the lost
    // update this exists to refuse.
    let second = serde_json::json!({ "menu.pho": { "en": "Pho noodles" } });
    let applied = router
        .clone()
        .oneshot(put_with_etag(&uri, &second, &cookie, &current))
        .await
        .expect("route the second publish");
    assert_eq!(applied.status(), StatusCode::NO_CONTENT);
    let replayed = router
        .clone()
        .oneshot(put_with_etag(&uri, &good, &cookie, &current))
        .await
        .expect("route the replay");
    assert_eq!(replayed.status(), StatusCode::PRECONDITION_FAILED);
    let survived = router
        .oneshot(get_with_cookie(&uri, &cookie))
        .await
        .expect("route the final read");
    assert_eq!(
        json_body(survived).await,
        second,
        "the refused replay did not overwrite the edit that won"
    );
}

/// A CSV import merges, so it retries around a competing save instead of refusing it (ADR-0095).
///
/// The two writers touch different keys. The import composes its rows onto whatever grid it read, so
/// when it loses the race the fix is to re-read and re-apply — not to hand an operator a conflict
/// about a key their CSV never mentioned. This is the same reasoning as the ten config node
/// publishes, and the opposite of `PUT /admin/translations`, which replaces a grid a human typed.
#[tokio::test]
async fn a_csv_import_survives_a_concurrent_save_of_a_different_key() {
    let translations = FakeTranslations::default();
    let router = translation_app(provisioned_admin(), translations.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let grid_uri = format!("/admin/translations?tenant_id={tenant_ulid}");

    let base = serde_json::json!({ "menu.pho": { "en": "Pho" } });
    let seeded = router
        .clone()
        .oneshot(put_with_etag(&grid_uri, &base, &cookie, "*"))
        .await
        .expect("route the seed");
    assert_eq!(seeded.status(), StatusCode::NO_CONTENT);

    // Somebody else saves `menu.rice` between this import's read and its write.
    *translations.interpose.lock().expect("lock") =
        Some(("menu.rice".to_owned(), "en".to_owned(), "Rice".to_owned()));

    let apply = router
        .clone()
        .oneshot(post_bytes_with_cookie(
            &format!("/admin/translations/import/apply?tenant_id={tenant_ulid}"),
            b"key,en\nmenu.tea,Tea\n".to_vec(),
            "text/csv",
            &cookie,
        ))
        .await
        .expect("route the apply");
    assert_eq!(
        apply.status(),
        StatusCode::OK,
        "an import that lost a race retries rather than refusing"
    );

    let merged = json_body(
        router
            .oneshot(get_with_cookie(&grid_uri, &cookie))
            .await
            .expect("route the read"),
    )
    .await;
    assert_eq!(
        merged["menu.tea"]["en"], "Tea",
        "the import's own key landed"
    );
    assert_eq!(
        merged["menu.rice"]["en"], "Rice",
        "and it did not clobber the save that beat it"
    );
    assert_eq!(merged["menu.pho"]["en"], "Pho", "nor the key already there");
}

#[tokio::test]
async fn translation_routes_require_a_session() {
    let router = translation_app(provisioned_admin(), FakeTranslations::default());
    let tenant_ulid = tenant().as_ulid().to_string();
    let response = router
        .oneshot(get(
            &format!("/admin/translations?tenant_id={tenant_ulid}"),
            None,
        ))
        .await
        .expect("route");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// The CSV export streams the grid as `text/csv` with a union-of-locales header and a row per key
/// (ADR-0075, Track M5); unauthenticated it is a 401.
#[tokio::test]
async fn translation_export_streams_csv_and_needs_a_session() {
    let router = translation_app(provisioned_admin(), FakeTranslations::default());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();

    let grid = serde_json::json!({
        "menu.pho": { "en": "Pho", "vi": "Phở" },
        "menu.tea": { "en": "Tea" },
    });
    let put = router
        .clone()
        .oneshot(put_with_etag(
            &format!("/admin/translations?tenant_id={tenant_ulid}"),
            &grid,
            &cookie,
            "*",
        ))
        .await
        .expect("route the publish");
    assert_eq!(put.status(), StatusCode::NO_CONTENT);

    let export_uri = format!("/admin/translations/export?tenant_id={tenant_ulid}");
    let unauth = router
        .clone()
        .oneshot(get(&export_uri, None))
        .await
        .expect("route");
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    let exported = router
        .oneshot(get_with_cookie(&export_uri, &cookie))
        .await
        .expect("route the export");
    assert_eq!(exported.status(), StatusCode::OK);
    let content_type = exported
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(content_type.starts_with("text/csv"), "served as CSV");
    let csv = text_body(exported).await;
    let mut lines = csv.lines();
    assert_eq!(lines.next().unwrap(), "key,en,vi");
    assert_eq!(lines.next().unwrap(), "menu.pho,Pho,Phở");
    // The tea key has no vi value, so its vi cell is an empty trailing field.
    assert_eq!(lines.next().unwrap(), "menu.tea,Tea,");
}

/// A CSV import is dry-run-first (ADR-0075, Track M5): the dry-run classifies every row and writes
/// nothing; the apply merges the valid rows onto the grid and skips the rejected ones.
#[tokio::test]
async fn translation_import_dry_runs_then_applies() {
    let router = translation_app(provisioned_admin(), FakeTranslations::default());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let grid_uri = format!("/admin/translations?tenant_id={tenant_ulid}");

    // A starting grid with one key.
    let base = serde_json::json!({ "menu.pho": { "en": "Pho" } });
    let put = router
        .clone()
        .oneshot(put_with_etag(&grid_uri, &base, &cookie, "*"))
        .await
        .expect("route the publish");
    assert_eq!(put.status(), StatusCode::NO_CONTENT);

    // A CSV that updates menu.pho, adds menu.tea, and has one row with no en (rejected).
    let csv = "key,en,vi\nmenu.pho,Pho noodles,Phở\nmenu.tea,Tea,Trà\nmenu.rice,,Cơm\n";

    // Dry-run: the report classifies the rows and the grid is untouched.
    let dry = router
        .clone()
        .oneshot(post_bytes_with_cookie(
            &format!("/admin/translations/import/dry-run?tenant_id={tenant_ulid}"),
            csv.as_bytes().to_vec(),
            "text/csv",
            &cookie,
        ))
        .await
        .expect("route the dry-run");
    assert_eq!(dry.status(), StatusCode::OK);
    let report = json_body(dry).await;
    assert_eq!(report["create_count"], 1);
    assert_eq!(report["update_count"], 1);
    assert_eq!(report["reject_count"], 1);
    let after_dry = router
        .clone()
        .oneshot(get_with_cookie(&grid_uri, &cookie))
        .await
        .expect("route the read");
    assert_eq!(json_body(after_dry).await, base, "a dry-run writes nothing");

    // Apply: the valid rows merge in; the rejected row is skipped.
    let apply = router
        .clone()
        .oneshot(post_bytes_with_cookie(
            &format!("/admin/translations/import/apply?tenant_id={tenant_ulid}"),
            csv.as_bytes().to_vec(),
            "text/csv",
            &cookie,
        ))
        .await
        .expect("route the apply");
    assert_eq!(apply.status(), StatusCode::OK);
    let applied = router
        .oneshot(get_with_cookie(&grid_uri, &cookie))
        .await
        .expect("route the re-read");
    let grid = json_body(applied).await;
    assert_eq!(grid["menu.pho"]["en"], "Pho noodles", "the update landed");
    assert_eq!(grid["menu.tea"]["vi"], "Trà", "the create landed");
    assert!(
        grid.get("menu.rice").is_none(),
        "the rejected row was skipped"
    );
}
