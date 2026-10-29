// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The fewest digits a PIN may have (ADR-0160 decision 2), on the PIN route and the session node.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud::config_tree::ConfigTreeStore;
use pos_cloud::http;
use pos_cloud::people::{EmployeeId, EmployeeStore, NewEmployee};
use pos_proto::ids::TenantId;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeConfigTrees, FakePeople, FakeRegistry, FakeSettings, FakeStoreGroups, LOCKOUT_ATTEMPTS,
    admin_cookie, clock, get_with_cookie, json_body, people_app_with_settings, provisioned_admin,
    put_number, put_with_cookie, seed_store, sent_to, settings_app, settings_brand, settings_group,
    settings_stores, store_id, tenant, tenant_layer_session,
};

// --- The fewest digits a PIN may have (ADR-0160 decision 2) ------------------------------------

const PIN_MIN_LENGTH: &str = "session.pin_min_length";

/// A settings store whose database is down: every call fails.
#[derive(Clone, Copy)]
struct UnreachableSettings;

impl pos_cloud::settings::SettingsStore for UnreachableSettings {
    async fn list(
        &self,
        _tenant_id: TenantId,
    ) -> Result<Vec<pos_cloud::settings::SettingValue>, pos_cloud::settings::SettingsStoreError>
    {
        Err(pos_cloud::settings::SettingsStoreError::new(
            "the database is down",
        ))
    }

    async fn put(
        &self,
        _tenant_id: TenantId,
        _value: &pos_cloud::settings::SettingValue,
    ) -> Result<(), pos_cloud::settings::SettingsStoreError> {
        Err(pos_cloud::settings::SettingsStoreError::new(
            "the database is down",
        ))
    }

    async fn delete(
        &self,
        _tenant_id: TenantId,
        _setting_key: &str,
        _scope: pos_proto::settings::SettingScope,
        _scope_id: &str,
    ) -> Result<bool, pos_cloud::settings::SettingsStoreError> {
        Err(pos_cloud::settings::SettingsStoreError::new(
            "the database is down",
        ))
    }
}

/// One person of the tenant with no PIN yet, for a test to set one for.
async fn a_person_without_a_pin(people: &FakePeople) -> EmployeeId {
    let employee_id = EmployeeId::new(Ulid::from_u128(0x919));
    EmployeeStore::create(
        people,
        &NewEmployee {
            employee_id,
            tenant_id: tenant(),
            code: "C09".to_owned(),
            name: "Alice".to_owned(),
        },
    )
    .await
    .expect("create employee");
    employee_id
}

/// Sets `pin` for `employee_id` through the PIN route.
async fn put_pin(
    router: &axum::Router,
    cookie: &str,
    employee_id: EmployeeId,
    pin: &str,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(put_with_cookie(
            &format!("/admin/employees/{employee_id}/pin"),
            &serde_json::json!({ "tenant_id": tenant().as_ulid().to_string(), "pin": pin }),
            cookie,
        ))
        .await
        .expect("route the PIN")
}

/// The message of the refusal a PIN of the wrong length earns, after checking it names the field.
async fn pin_refusal(response: axum::response::Response) -> String {
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["error"]["details"][0]["field"], "pin", "{body}");
    assert_eq!(
        body["error"]["details"][0]["reason"], "OUT_OF_RANGE",
        "{body}"
    );
    body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Whether `employee_id` has a PIN, as the console's read of one person says.
async fn has_pin(router: &axum::Router, cookie: &str, employee_id: EmployeeId) -> bool {
    let person = json_body(
        router
            .clone()
            .oneshot(get_with_cookie(
                &format!(
                    "/admin/employees/{employee_id}?tenant_id={}",
                    tenant().as_ulid()
                ),
                cookie,
            ))
            .await
            .expect("route the read"),
    )
    .await;
    person["has_pin"] == serde_json::json!(true)
}

/// A PIN is four to eight digits until the tenant writes a minimum. From then on a PIN set or reset
/// has at least that many digits, and never more than eight, and the refusal names the range. A PIN
/// set before is left as it was. The minimum is written through the settings routes and read by the
/// PIN route from the same store, as `main.rs` wires them.
#[tokio::test]
async fn a_pin_set_once_the_tenant_asks_for_more_digits_has_at_least_that_many() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let settings = FakeSettings::default();
    let registry = FakeRegistry::default();
    seed_store(&registry, tenant(), store_id(), true);
    let config = FakeConfigTrees::default();
    let router = people_app_with_settings(
        admin.clone(),
        people.clone(),
        registry.clone(),
        config.clone(),
        settings.clone(),
        Arc::new(NoopAuditRecorder),
    )
    .merge(http::settings_router(
        &pos_cloud::countries::registry(),
        settings,
        registry,
        FakeStoreGroups::default(),
        config,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ));
    let cookie = admin_cookie(&router).await;
    let person = a_person_without_a_pin(&people).await;

    // Nothing written: four to eight digits, as before.
    for pin in ["123", "123456789"] {
        assert_eq!(
            pin_refusal(put_pin(&router, &cookie, person, pin).await).await,
            "the PIN must be 4 to 8 digits",
            "{pin}"
        );
    }
    let four = put_pin(&router, &cookie, person, "1234").await;
    assert_eq!(four.status(), StatusCode::NO_CONTENT);

    let written = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/settings",
            &put_number(PIN_MIN_LENGTH, &serde_json::json!(6)),
            &cookie,
        ))
        .await
        .expect("route the write");
    assert_eq!(written.status(), StatusCode::OK);

    // The four-digit PIN is not touched: the store goes on taking it until it is reset.
    assert!(has_pin(&router, &cookie, person).await);
    for pin in ["12345", "123456789"] {
        assert_eq!(
            pin_refusal(put_pin(&router, &cookie, person, pin).await).await,
            "the PIN must be 6 to 8 digits",
            "{pin}"
        );
    }
    for pin in ["123456", "12345678"] {
        let set = put_pin(&router, &cookie, person, pin).await;
        assert_eq!(set.status(), StatusCode::NO_CONTENT, "{pin}");
    }
}

/// A PIN is never held to four when the tenant's minimum cannot be read: the route answers as it
/// does for any store it cannot reach, and no PIN is set.
#[tokio::test]
async fn a_pin_is_refused_while_the_tenants_settings_cannot_be_read() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let registry = FakeRegistry::default();
    seed_store(&registry, tenant(), store_id(), true);
    let router = people_app_with_settings(
        admin,
        people.clone(),
        registry,
        FakeConfigTrees::default(),
        UnreachableSettings,
        Arc::new(NoopAuditRecorder),
    );
    let cookie = admin_cookie(&router).await;
    let person = a_person_without_a_pin(&people).await;

    let refused = put_pin(&router, &cookie, person, "12345678").await;
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["status"], "UNAVAILABLE", "{body}");
    assert!(!has_pin(&router, &cookie, person).await, "no PIN was set");
}

/// The minimum is the tenant's alone, because a person may work at every store: it cannot be
/// written for one store, a brand or a group. Written for every store, it reaches each store's
/// `session` node beside the other session settings, and the node still reads as the edge reads
/// it.
#[tokio::test]
async fn the_pin_minimum_is_written_for_the_whole_tenant_and_rides_the_session_node() {
    let (router, config_trees, _) = settings_app();
    let cookie = admin_cookie(&router).await;
    let [first, _, _] = settings_stores();

    for (scope, scope_id) in [
        ("SETTING_SCOPE_STORE", first.to_string()),
        ("SETTING_SCOPE_BRAND", settings_brand().to_string()),
        ("SETTING_SCOPE_STORE_GROUP", settings_group().to_string()),
    ] {
        let refused = router
            .clone()
            .oneshot(put_with_cookie(
                "/admin/settings",
                &serde_json::json!({
                    "tenant_id": tenant().as_ulid().to_string(),
                    "setting_key": PIN_MIN_LENGTH,
                    "scope": scope,
                    "scope_id": scope_id,
                    "value": 6,
                }),
                &cookie,
            ))
            .await
            .expect("route the write");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST, "{scope}");
        let body = json_body(refused).await;
        assert_eq!(body["error"]["details"][0]["field"], "scope", "{scope}");
        assert_eq!(
            body["error"]["details"][0]["reason"], "SCOPE_NOT_ALLOWED",
            "{scope}"
        );
    }
    for store_id in settings_stores() {
        assert_eq!(tenant_layer_session(&config_trees, store_id).await, None);
    }

    for (setting_key, value) in [(PIN_MIN_LENGTH, 6), (LOCKOUT_ATTEMPTS, 3)] {
        let written = router
            .clone()
            .oneshot(put_with_cookie(
                "/admin/settings",
                &put_number(setting_key, &serde_json::json!(value)),
                &cookie,
            ))
            .await
            .expect("route the write");
        assert_eq!(written.status(), StatusCode::OK, "{setting_key}");
    }
    for store_id in settings_stores() {
        let tree = config_trees
            .load(tenant(), store_id)
            .await
            .expect("load")
            .expect("a tree");
        let sent = sent_to(&tree.record)["session"].clone();
        assert_eq!(
            sent,
            serde_json::json!({ "lockout_attempts": 3, "pin_min_length": 6 })
        );
        let session: pos_proto::session::PublishedSession =
            serde_json::from_value(sent).expect("the edge reads the node");
        assert_eq!(session.pin_min_length(), 6);
        assert_eq!(session.lockout_attempts(), 3, "the lockout is unaffected");
        assert_eq!(session.lockout_minutes(), 5);
    }
}
