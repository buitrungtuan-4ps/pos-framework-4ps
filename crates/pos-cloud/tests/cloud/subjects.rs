// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Subject-request tooling (`/admin/subjects`, ADR-0076): owner-only, audited lookup, export and
//! erasure.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::retention::{RetentionError, SubjectRecord, SubjectStore};
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::ids::{SubjectId, TenantId};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeConfigTrees, FakeKeys, FakeRollups, FakeWebhooks, NOW_MS, app_all, clock, get,
    get_with_cookie, json_body, post_with_cookie, provisioned_admin, role_session_cookie, tenant,
};

// --- Subject-request tooling (`/admin/subjects`, owner-only, ADR-0076) ---------------------------

/// An in-memory subject store: rows keyed by `(tenant, record)`. Only `fetch` and `save_masked` are
/// exercised by the tooling; `due_before` is the cron's and returns nothing here.
#[derive(Clone, Default)]
struct FakeSubjects {
    rows: Arc<Mutex<Vec<(TenantId, SubjectRecord)>>>,
}

impl FakeSubjects {
    fn with(tenant: TenantId, record: SubjectRecord) -> Self {
        Self {
            rows: Arc::new(Mutex::new(vec![(tenant, record)])),
        }
    }
}

impl SubjectStore for FakeSubjects {
    async fn due_before(
        &self,
        _cutoff: Timestamp,
        _limit: u32,
    ) -> Result<Vec<SubjectRecord>, RetentionError> {
        Ok(Vec::new())
    }

    async fn save_masked(&self, masked: &[SubjectRecord]) -> Result<u64, RetentionError> {
        let mut rows = self.rows.lock().expect("lock");
        let mut saved = 0;
        for update in masked {
            if let Some(entry) = rows
                .iter_mut()
                .find(|(_, row)| row.subject_id == update.subject_id)
            {
                entry.1 = update.clone();
                saved += 1;
            }
        }
        Ok(saved)
    }

    async fn fetch(
        &self,
        tenant: TenantId,
        subject_id: SubjectId,
    ) -> Result<Option<SubjectRecord>, RetentionError> {
        let rows = self.rows.lock().expect("lock");
        Ok(rows
            .iter()
            .find(|(row_tenant, row)| *row_tenant == tenant && row.subject_id == subject_id)
            .map(|(_, row)| row.clone()))
    }
}

/// The main router (for the session guard) plus the subject-request sub-router.
fn subjects_app(admin: FakeAdmin, subjects: FakeSubjects) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::subjects_router(
        subjects,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

/// A synthetic subject record — placeholder values, never anything resembling a real person.
fn a_subject(subject_id: SubjectId) -> SubjectRecord {
    SubjectRecord {
        subject_id,
        collected_at: Timestamp::from_milliseconds_since_epoch(NOW_MS).expect("valid"),
        fields: BTreeMap::from([
            ("name".to_owned(), "Test Subject".to_owned()),
            ("phone".to_owned(), "N/A".to_owned()),
        ]),
        masked_at: None,
    }
}

/// Lookup returns status without values; export returns values; erase masks; and every route is
/// owner-only (ADR-0076). An admin (not owner) is refused.
#[tokio::test]
async fn subject_lookup_export_and_erase_are_owner_only_and_audited() {
    let admin = provisioned_admin();
    let subject_id = SubjectId::new(Ulid::from_u128(0x0ABC));
    let subjects = FakeSubjects::with(tenant(), a_subject(subject_id));
    let router = subjects_app(admin.clone(), subjects);
    let owner = role_session_cookie(&admin, AdminRole::Owner, "owner-token").await;
    let non_owner = role_session_cookie(&admin, AdminRole::Admin, "admin-token").await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let sid = subject_id.to_string();
    let lookup_uri = format!("/admin/subjects/{sid}?tenant_id={tenant_ulid}");

    // An admin (not owner) is refused — owner-only, a 403 distinct from the 401 of no session.
    let denied = router
        .clone()
        .oneshot(get_with_cookie(&lookup_uri, &non_owner))
        .await
        .expect("route");
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);

    // The owner looks up: metadata only, no field values.
    let looked = router
        .clone()
        .oneshot(get_with_cookie(&lookup_uri, &owner))
        .await
        .expect("route");
    assert_eq!(looked.status(), StatusCode::OK);
    let meta = json_body(looked).await;
    assert_eq!(meta["masked"], false);
    assert_eq!(meta["field_count"], 2);
    assert!(meta.get("fields").is_none(), "a lookup returns no values");

    // Export returns the field values — the portability payload.
    let exported = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/subjects/{sid}/export?tenant_id={tenant_ulid}"),
            &owner,
        ))
        .await
        .expect("route");
    assert_eq!(exported.status(), StatusCode::OK);
    assert_eq!(json_body(exported).await["fields"]["name"], "Test Subject");

    // Erase masks the record.
    let erased = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/subjects/{sid}/erase?tenant_id={tenant_ulid}"),
            &serde_json::json!({}),
            &owner,
        ))
        .await
        .expect("route");
    assert_eq!(erased.status(), StatusCode::OK);
    assert_eq!(json_body(erased).await["already_masked"], false);

    // After erasure the lookup shows masked, and an export returns the redaction sentinel.
    let after = router
        .clone()
        .oneshot(get_with_cookie(&lookup_uri, &owner))
        .await
        .expect("route");
    assert_eq!(json_body(after).await["masked"], true);
    let re_export = router
        .oneshot(get_with_cookie(
            &format!("/admin/subjects/{sid}/export?tenant_id={tenant_ulid}"),
            &owner,
        ))
        .await
        .expect("route");
    assert_eq!(
        json_body(re_export).await["fields"]["name"],
        "[REDACTED]",
        "erasure masked the value"
    );
}

/// An unknown id is a 404, and the routes need a session.
#[tokio::test]
async fn subject_lookup_404s_for_an_unknown_id_and_needs_a_session() {
    let admin = provisioned_admin();
    let router = subjects_app(admin.clone(), FakeSubjects::default());
    let owner = role_session_cookie(&admin, AdminRole::Owner, "owner-token").await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let sid = Ulid::from_u128(0x0999).to_string();
    let uri = format!("/admin/subjects/{sid}?tenant_id={tenant_ulid}");

    let missing = router
        .clone()
        .oneshot(get_with_cookie(&uri, &owner))
        .await
        .expect("route");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let no_session = router.oneshot(get(&uri, None)).await.expect("route");
    assert_eq!(no_session.status(), StatusCode::UNAUTHORIZED);
}
