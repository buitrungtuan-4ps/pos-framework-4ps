// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Fleet liveness (ADR-0068): the fleet read, a store's region difference and its acknowledgement,
//! and the session the reads need.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::{AuditRecorder, AuditSink, NoopAuditRecorder};
use pos_cloud::fleet::FleetRow;
use pos_cloud::lease::{Region, StorePlacement};
use pos_cloud::registry::EntityStatus;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeAudit, FakeConfigTrees, FakeFleet, NOW_MS, admin_cookie, fleet_app, fleet_app_with_audit,
    fleet_app_with_config, get, get_with_cookie, json_body, post_with_cookie, provisioned_admin,
    seen_ago, store_id, store_in_handover, tenant,
};

/// The two fleet rows the listing test reads: one healthy, one that is offline, drifted, backed up
/// **and** replaced. Lifted out of the test so the assertions stay the readable part.
fn drifted_fleet(online_store: StoreId, offline_store: StoreId) -> FakeFleet {
    FakeFleet::default()
        .with_row(
            tenant(),
            FleetRow {
                store_id: online_store,
                name: "Bến Thành".to_owned(),
                status: EntityStatus::Active,
                last_seen_at: Some(seen_ago(1_000)),
                last_config_pull_at: Some(seen_ago(1_000)),
                config_version_held: Some("v-current".to_owned()),
                config_version_published: Some("v-current".to_owned()),
                relay_backlog: 0,
                relay_oldest_pending_at: None,
                installed_version: Some("v1.2.3".to_owned()),
                self_test_ok: Some(true),
                reported_at: Some(seen_ago(1_000)),
                outbox_depth: Some(0),
                outbox_reported_at: Some(seen_ago(1_000)),
                // Under a lease and holding the current generation: active, and not "superseded".
                lease_generation_held: Some(3),
                lease_reported_at: Some(seen_ago(1_000)),
                lease_generation_authoritative: Some(3),
                edge_placement: StorePlacement::NeverIssued,
                region: None,
                superseded_generation: None,
                retired_at: None,
                retired_by: None,
                print_agents: None,
                print_agents_reported_at: None,
            },
        )
        .with_row(
            tenant(),
            FleetRow {
                store_id: offline_store,
                name: "Xuân Thủy".to_owned(),
                status: EntityStatus::Active,
                last_seen_at: Some(seen_ago(600_000)),
                last_config_pull_at: Some(seen_ago(600_000)),
                config_version_held: Some("v-old".to_owned()),
                config_version_published: Some("v-current".to_owned()),
                relay_backlog: 3,
                relay_oldest_pending_at: Some(seen_ago(120_000)),
                installed_version: None,
                self_test_ok: None,
                reported_at: None,
                outbox_depth: Some(41),
                outbox_reported_at: Some(seen_ago(600_000)),
                // Holds generation 2 while the store's authority is on 3: this box has been
                // replaced, and the console must say so rather than only "offline" (ADR-0108).
                lease_generation_held: Some(2),
                lease_reported_at: Some(seen_ago(600_000)),
                lease_generation_authoritative: Some(3),
                edge_placement: StorePlacement::NeverIssued,
                region: None,
                superseded_generation: None,
                retired_at: None,
                retired_by: None,
                print_agents: None,
                print_agents_reported_at: None,
            },
        )
}

/// The handover state reaches the console as a word, and `null` really is one of the answers
/// ([ADR-0110](../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
///
/// The `null` cases are the ones worth a route test rather than only a unit test: they are what the
/// console renders for most of the fleet, and reading ADR-0110's `taking-over` definition literally
/// would have put a mid-handover badge on every one of them.
/// The single-store read compares a store's region against the country it publishes, and says
/// "country not recorded" rather than letting absence read as agreement
/// ([ADR-0114](../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// Three answers and no fourth, derived on the server so the console never re-implements the rule.
/// The third is the one that matters: a store that has never published a country has *nothing* to
/// compare, and rendering that as agreement would be the exact lie this record exists to prevent.
#[tokio::test]
async fn the_single_store_read_compares_the_region_against_the_stores_own_country() {
    let store = store_id();
    let tenant_ulid = tenant().as_ulid().to_string();

    // Three routers over the same fleet row — a store hosted in Singapore — differing only in what
    // country the store has published.
    let hosted = || {
        let mut row = store_in_handover(store, Some(1), None, None);
        row.region = Region::stored("SG", "ap-southeast-1");
        FakeFleet::default().with_row(tenant(), row)
    };
    let published = |country: Option<&str>| {
        let trees = FakeConfigTrees::default();
        if let Some(country) = country {
            trees.seed_store_layer(
                tenant(),
                store,
                serde_json::json!({ "locale": { "country_code": country } }),
            );
        }
        trees
    };

    // (what the store publishes, the agreement it must report, the country it must echo)
    let cases: [(Option<&str>, &str, serde_json::Value); 3] = [
        (Some("SG"), "agrees", serde_json::json!("SG")),
        (Some("VN"), "differs", serde_json::json!("VN")),
        (None, "country-not-recorded", serde_json::Value::Null),
    ];

    for (country, expected, echoed) in cases {
        let router = fleet_app_with_config(provisioned_admin(), hosted(), published(country));
        let cookie = admin_cookie(&router).await;
        let response = router
            .clone()
            .oneshot(get_with_cookie(
                &format!("/admin/fleet/{store}?tenant_id={tenant_ulid}"),
                &cookie,
            ))
            .await
            .expect("route the single-store read");
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(
            body["region_agreement"], expected,
            "a store in SG whose published country is {country:?}"
        );
        assert_eq!(body["profile_country"], echoed);
        assert_eq!(
            body["region_country"], "SG",
            "and the region itself is reported either way — the comparison never hides it"
        );
    }
}

/// A person can answer the region warning, the answer reaches the read, and the trail records who
/// ([ADR-0114](../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// The whole point of the record: a mismatch that warns forever with no way to say why it is correct
/// is a warning people learn to ignore. Three things have to be true together — the row is written,
/// the read reports it standing, and the audit trail carries the reason with a name against it.
#[tokio::test]
async fn a_region_difference_can_be_answered_and_the_answer_is_audited() {
    let store = store_id();
    let tenant_ulid = tenant().as_ulid().to_string();
    let mut row = store_in_handover(store, Some(1), None, None);
    row.region = Region::stored("SG", "ap-southeast-1");
    let fleet = FakeFleet::default().with_row(tenant(), row);
    let trees = FakeConfigTrees::default();
    trees.seed_store_layer(
        tenant(),
        store,
        serde_json::json!({ "locale": { "country_code": "VN" } }),
    );
    let audit = FakeAudit::default();
    let sink: Arc<dyn AuditRecorder> = Arc::new(AuditSink::new(audit.clone()));
    let router = fleet_app_with_audit(provisioned_admin(), fleet, trees, sink);
    let cookie = admin_cookie(&router).await;

    // Before: the difference is visible and unanswered.
    let before = json_body(
        router
            .clone()
            .oneshot(get_with_cookie(
                &format!("/admin/fleet/{store}?tenant_id={tenant_ulid}"),
                &cookie,
            ))
            .await
            .expect("route the read"),
    )
    .await;
    assert_eq!(before["region_agreement"], "differs");
    assert!(
        before.get("region_acknowledgement").is_none(),
        "an unanswered difference omits the key entirely rather than sending null"
    );

    let answered = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/stores/{store}/region-acknowledgement"),
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "profile_country": "VN",
                "region_country": "SG",
                "reason": "Customer consent recorded at sign-up; DTA in force.",
            }),
            &cookie,
        ))
        .await
        .expect("route the acknowledgement");
    assert_eq!(answered.status(), StatusCode::OK);

    // After: the same difference now reads as answered, carrying the reason as typed.
    let after = json_body(
        router
            .oneshot(get_with_cookie(
                &format!("/admin/fleet/{store}?tenant_id={tenant_ulid}"),
                &cookie,
            ))
            .await
            .expect("route the read again"),
    )
    .await;
    assert_eq!(
        after["region_agreement"], "differs",
        "an answer explains the difference; it does not erase it"
    );
    assert_eq!(
        after["region_acknowledgement"]["reason"],
        "Customer consent recorded at sign-up; DTA in force."
    );
    assert_eq!(after["region_acknowledgement"]["profile_country"], "VN");
    assert_eq!(after["region_acknowledgement"]["region_country"], "SG");

    let entries = audit.entries.lock().expect("lock").clone();
    let entry = entries
        .iter()
        .find(|entry| entry.action == "store.edge_placement.acknowledge")
        .expect("the acknowledgement is audited");
    assert_eq!(entry.entity_type, "store");
    assert_eq!(entry.entity_id, store.to_string());
    let after_detail = entry.after.as_ref().expect("the detail");
    assert_eq!(
        after_detail["reason"], "Customer consent recorded at sign-up; DTA in force.",
        "the reason is carried whole — a trail that summarises it answers nothing"
    );
    assert_eq!(after_detail["profile_country"], "VN");
    assert_eq!(after_detail["region_country"], "SG");
    assert!(
        !entry.actor.email.is_empty(),
        "the trail carries who decided; the row deliberately does not"
    );
}

/// An answer is about **one** difference: it stops standing the moment either country moves
/// ([ADR-0114](../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// This is the property the stored country pair exists for, and it is the one a simpler design
/// would get wrong: a boolean "acknowledged" flag would silence the warning for a store that has
/// since been moved somewhere nobody approved. Nothing clears the row — it stops matching, which is
/// the same thing and cannot be forgotten.
#[tokio::test]
async fn an_answer_stops_standing_when_the_store_moves_somewhere_else() {
    let store = store_id();
    let tenant_ulid = tenant().as_ulid().to_string();
    let hosted_in = |country: &str| {
        let mut row = store_in_handover(store, Some(1), None, None);
        row.region = Region::stored(country, "a rack somewhere");
        FakeFleet::default().with_row(tenant(), row)
    };
    let trees = FakeConfigTrees::default();
    trees.seed_store_layer(
        tenant(),
        store,
        serde_json::json!({ "locale": { "country_code": "VN" } }),
    );

    // Answer the SG-versus-VN difference.
    let router = fleet_app_with_audit(
        provisioned_admin(),
        hosted_in("SG"),
        trees.clone(),
        Arc::new(NoopAuditRecorder),
    );
    let cookie = admin_cookie(&router).await;
    let answered = router
        .oneshot(post_with_cookie(
            &format!("/admin/stores/{store}/region-acknowledgement"),
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "profile_country": "VN",
                "region_country": "SG",
                "reason": "Approved for Singapore.",
            }),
            &cookie,
        ))
        .await
        .expect("route the acknowledgement");
    assert_eq!(answered.status(), StatusCode::OK);

    // The same tenant, the same store, the same stored answer — but the machine is now in Japan.
    let moved = fleet_app_with_audit(
        provisioned_admin(),
        hosted_in("JP"),
        trees.clone(),
        Arc::new(NoopAuditRecorder),
    );
    let cookie = admin_cookie(&moved).await;
    let body = json_body(
        moved
            .clone()
            .oneshot(get_with_cookie(
                &format!("/admin/fleet/{store}?tenant_id={tenant_ulid}"),
                &cookie,
            ))
            .await
            .expect("route the read"),
    )
    .await;
    assert_eq!(body["region_agreement"], "differs");
    assert!(
        body.get("region_acknowledgement").is_none(),
        "an answer about Singapore must not silence a warning about Japan"
    );

    // And a second acknowledgement naming the *old* pair is refused rather than recorded: the admin
    // is answering a screen that no longer describes the store.
    let stale = moved
        .oneshot(post_with_cookie(
            &format!("/admin/stores/{store}/region-acknowledgement"),
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "profile_country": "VN",
                "region_country": "SG",
                "reason": "Approved for Singapore.",
            }),
            &cookie,
        ))
        .await
        .expect("route the stale acknowledgement");
    assert_eq!(stale.status(), StatusCode::CONFLICT);
}

/// The reason is required, bounded, and counted in characters
/// ([ADR-0114](../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// A warning silenced with no explanation is a warning deleted. The ceiling is counted in Unicode
/// scalar values so a Vietnamese or Japanese reason is measured the way the person who typed it
/// would measure it — 280 three-byte characters is a sentence, not an over-long one.
#[tokio::test]
async fn the_reason_is_required_and_its_ceiling_counts_characters() {
    let store = store_id();
    let tenant_ulid = tenant().as_ulid().to_string();
    let mut row = store_in_handover(store, Some(1), None, None);
    row.region = Region::stored("SG", "ap-southeast-1");
    let trees = FakeConfigTrees::default();
    trees.seed_store_layer(
        tenant(),
        store,
        serde_json::json!({ "locale": { "country_code": "VN" } }),
    );
    let router = fleet_app_with_audit(
        provisioned_admin(),
        FakeFleet::default().with_row(tenant(), row),
        trees,
        Arc::new(NoopAuditRecorder),
    );
    let cookie = admin_cookie(&router).await;
    let acknowledge = |reason: serde_json::Value| {
        let mut body = serde_json::json!({
            "tenant_id": tenant_ulid.clone(),
            "profile_country": "VN",
            "region_country": "SG",
        });
        if let serde_json::Value::Object(map) = &mut body
            && !reason.is_null()
        {
            map.insert("reason".to_owned(), reason);
        }
        post_with_cookie(
            &format!("/admin/stores/{store}/region-acknowledgement"),
            &body,
            &cookie,
        )
    };

    // Absent, blank, and whitespace-only are the same refusal — and absent goes through the
    // envelope rather than axum's bare 422, which is what `#[serde(default)]` buys.
    for reason in [
        serde_json::Value::Null,
        serde_json::json!(""),
        serde_json::json!("   "),
    ] {
        let response = router
            .clone()
            .oneshot(acknowledge(reason.clone()))
            .await
            .expect("route the acknowledgement");
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "a warning silenced with no explanation is a warning deleted: {reason:?}"
        );
        let body = json_body(response).await;
        assert_eq!(body["error"]["details"][0]["field"], "reason");
    }

    // 280 Vietnamese characters is 840 bytes; a byte ceiling would refuse a sentence a person can
    // read at a glance.
    let at_the_ceiling: String = "ộ".repeat(280);
    assert!(at_the_ceiling.len() > 280, "the test string is multi-byte");
    let accepted = router
        .clone()
        .oneshot(acknowledge(serde_json::json!(at_the_ceiling)))
        .await
        .expect("route the acknowledgement");
    assert_eq!(accepted.status(), StatusCode::OK);

    let over = router
        .oneshot(acknowledge(serde_json::json!("ộ".repeat(281))))
        .await
        .expect("route the acknowledgement");
    assert_eq!(over.status(), StatusCode::BAD_REQUEST);
    let body = json_body(over).await;
    assert_eq!(body["error"]["details"][0]["field"], "reason");
}

/// A store with nothing to answer cannot be acknowledged
/// ([ADR-0114](../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// Refused rather than stored, because a row saying somebody approved a transfer that is not
/// happening is a record of a decision nobody made — and a fleet that accumulates them teaches its
/// readers that the record means nothing.
#[tokio::test]
async fn a_store_with_no_difference_has_nothing_to_acknowledge() {
    let store = store_id();
    let tenant_ulid = tenant().as_ulid().to_string();
    let trees = FakeConfigTrees::default();
    trees.seed_store_layer(
        tenant(),
        store,
        serde_json::json!({ "locale": { "country_code": "VN" } }),
    );
    // (what the store's region is, what the two codes named in the request are)
    let cases: [(Option<&str>, &str); 2] = [
        // Agreeing: hosted in the country the store is registered in.
        (Some("VN"), "VN"),
        // No region at all: an in-store machine, which is not a transfer anybody decided on.
        (None, "SG"),
    ];
    for (region, claimed) in cases {
        let mut row = store_in_handover(store, Some(1), None, None);
        row.region = region.and_then(|country| Region::stored(country, "a rack somewhere"));
        let router = fleet_app_with_audit(
            provisioned_admin(),
            FakeFleet::default().with_row(tenant(), row),
            trees.clone(),
            Arc::new(NoopAuditRecorder),
        );
        let cookie = admin_cookie(&router).await;
        let response = router
            .oneshot(post_with_cookie(
                &format!("/admin/stores/{store}/region-acknowledgement"),
                &serde_json::json!({
                    "tenant_id": tenant_ulid,
                    "profile_country": "VN",
                    "region_country": claimed,
                    "reason": "There is nothing here to approve.",
                }),
                &cookie,
            ))
            .await
            .expect("route the acknowledgement");
        assert_eq!(
            response.status(),
            StatusCode::CONFLICT,
            "a store whose region is {region:?} has no difference to answer"
        );
    }
}

/// An in-store store has no region, so there is nothing to compare and no badge to draw
/// ([ADR-0114](../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// `null` here is a real answer and deliberately *not* `agrees`: an in-store machine and a hosted
/// machine that happens to be in the right country are different facts, and only one of them is a
/// transfer somebody decided on.
#[tokio::test]
async fn a_store_with_no_region_reports_no_agreement_rather_than_agreeing() {
    let store = store_id();
    let tenant_ulid = tenant().as_ulid().to_string();
    let fleet =
        FakeFleet::default().with_row(tenant(), store_in_handover(store, Some(1), None, None));
    let trees = FakeConfigTrees::default();
    trees.seed_store_layer(
        tenant(),
        store,
        serde_json::json!({ "locale": { "country_code": "VN" } }),
    );

    let router = fleet_app_with_config(provisioned_admin(), fleet, trees);
    let cookie = admin_cookie(&router).await;
    let body = json_body(
        router
            .clone()
            .oneshot(get_with_cookie(
                &format!("/admin/fleet/{store}?tenant_id={tenant_ulid}"),
                &cookie,
            ))
            .await
            .expect("route the single-store read"),
    )
    .await;
    assert!(body["region_country"].is_null());
    assert!(
        body["region_agreement"].is_null(),
        "an in-store machine is in the shop; there is no transfer to agree or disagree about"
    );
}

/// Where a store's data rests reaches the console on the read it already runs
/// ([ADR-0114](../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// The fleet page is where somebody is already looking when the question is asked, and the region
/// rides the `LEFT JOIN store_lease` the placement beside it already uses — so this costs no extra
/// query and no work that grows with the fleet.
#[tokio::test]
async fn the_fleet_read_carries_where_each_store_rests_and_says_nothing_when_it_has_no_region() {
    let hosted = StoreId::new(Ulid::from_u128(0x00F1_EE91));
    let in_store = StoreId::new(Ulid::from_u128(0x00F1_EE92));

    let mut abroad = store_in_handover(hosted, Some(1), None, None);
    abroad.region = Region::stored("SG", "ap-southeast-1");
    // A store in its own shop: the machine is in the shop, so there is no region and there must not
    // be one. Left as the fixture's `None`, which is what the column holds.
    let home = store_in_handover(in_store, Some(1), None, None);

    let fleet = FakeFleet::default()
        .with_row(tenant(), abroad)
        .with_row(tenant(), home);
    let router = fleet_app(provisioned_admin(), fleet);
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();

    let response = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/fleet?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the fleet read");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let rows = body.as_array().expect("an array of stores");
    let row = |wanted: StoreId| {
        rows.iter()
            .find(|row| row["store_id"] == wanted.to_string())
            .expect("the store is in the listing")
            .clone()
    };

    assert_eq!(row(hosted)["region_country"], "SG");
    assert_eq!(row(hosted)["region_label"], "ap-southeast-1");
    assert!(
        row(in_store)["region_country"].is_null() && row(in_store)["region_label"].is_null(),
        "an in-store machine is in the shop and has no region; a fabricated one would answer the \
         transfer question with a lie"
    );

    // And the single-store read carries it too, because that is the one the store hub rides
    // (ADR-0099: each card reads an endpoint that already exists).
    let detail = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/fleet/{hosted}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the single-store read");
    assert_eq!(detail.status(), StatusCode::OK);
    let detail = json_body(detail).await;
    assert_eq!(detail["region_country"], "SG");
    assert_eq!(detail["region_label"], "ap-southeast-1");
}

#[tokio::test]
async fn the_fleet_read_reports_a_handover_state_and_nothing_for_a_store_that_never_had_one() {
    let never = StoreId::new(Ulid::from_u128(0x00F1_EE81));
    let first = StoreId::new(Ulid::from_u128(0x00F1_EE82));
    let moving = StoreId::new(Ulid::from_u128(0x00F1_EE83));
    let done = StoreId::new(Ulid::from_u128(0x00F1_EE84));
    let gone = StoreId::new(Ulid::from_u128(0x00F1_EE85));
    let retired_at = seen_ago(5_000);

    let fleet = FakeFleet::default()
        // No lease row at all — most of the fleet.
        .with_row(tenant(), store_in_handover(never, None, None, None))
        // Generation 0: a first-ever lease, which supersedes nobody.
        .with_row(tenant(), store_in_handover(first, Some(0), None, None))
        // A bump displaced generation 0 and nothing has proved it drained.
        .with_row(tenant(), store_in_handover(moving, Some(1), Some(0), None))
        // The same store once the outgoing machine reported empty at the right generation.
        .with_row(tenant(), store_in_handover(done, Some(1), None, None))
        // And once a person decided the old box is no longer needed.
        .with_row(
            tenant(),
            store_in_handover(gone, Some(1), None, Some(retired_at)),
        );

    let router = fleet_app(provisioned_admin(), fleet);
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let response = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/fleet?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the fleet read");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let rows = body.as_array().expect("an array of stores");
    let state = |wanted: StoreId| {
        rows.iter()
            .find(|row| row["store_id"] == wanted.to_string())
            .expect("the store is in the listing")["handover"]
            .clone()
    };

    assert_eq!(
        state(never),
        serde_json::Value::Null,
        "a store with no lease has never handed anything over"
    );
    assert_eq!(
        state(first),
        serde_json::Value::Null,
        "generation 0 is a first lease, not a handover — the console renders no badge"
    );
    assert_eq!(state(moving), "taking-over");
    assert_eq!(state(done), "settled");
    assert_eq!(state(gone), "retired");

    // The retirement's two facts travel with the state, so a console can say who and when without a
    // second read — and the id, never the address.
    let row = rows
        .iter()
        .find(|row| row["store_id"] == gone.to_string())
        .expect("the retired store");
    assert_eq!(
        row["retired_at_ms"],
        retired_at.as_milliseconds_since_epoch()
    );
    assert_eq!(row["retired_by"], "01ADMINADMINADMINADMINADMI");
}

#[tokio::test]
async fn fleet_lists_stores_with_online_and_config_drift_derived_at_read() {
    let online_store = StoreId::new(Ulid::from_u128(0x00F1_EE7A));
    let offline_store = StoreId::new(Ulid::from_u128(0x00F1_EE7B));
    // One store seen a second ago, holding the published version and the current lease — online, in
    // sync, and still the machine the shop runs on. One seen ten minutes ago, holding an old config
    // version and an old lease generation, with a relay backlog — offline, drifted, and replaced.
    let fleet = drifted_fleet(online_store, offline_store);
    let router = fleet_app(provisioned_admin(), fleet);
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();

    let listed = router
        .oneshot(get_with_cookie(
            &format!("/admin/fleet?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the fleet list");
    assert_eq!(listed.status(), StatusCode::OK);
    let body = json_body(listed).await;
    let rows = body.as_array().expect("an array of stores");
    assert_eq!(rows.len(), 2, "both of the tenant's stores are listed");

    let online = &rows[0];
    assert_eq!(online["store_id"], online_store.as_ulid().to_string());
    assert_eq!(online["name"], "Bến Thành");
    assert_eq!(online["online"], true, "seen a second ago reads as online");
    assert_eq!(
        online["config_current"], true,
        "held equals published, so it is current"
    );
    assert_eq!(online["relay_backlog"], 0);
    assert_eq!(
        online["installed_version"], "v1.2.3",
        "the fleet read surfaces the reported OTA version"
    );
    assert_eq!(online["self_test_ok"], true, "and its self-test outcome");
    assert_eq!(
        online["outbox_depth"], 0,
        "a store that reported an empty outbox has nothing waiting to publish"
    );
    assert_eq!(
        online["lease_superseded"], false,
        "a box holding its store's current generation is still the machine the shop runs on"
    );

    let offline = &rows[1];
    assert_eq!(
        offline["online"], false,
        "seen ten minutes ago is past the freshness window"
    );
    assert_eq!(
        offline["config_current"], false,
        "holding an old version is a drift"
    );
    assert_eq!(offline["config_version_held"], "v-old");
    assert_eq!(offline["config_version_published"], "v-current");
    assert_eq!(offline["relay_backlog"], 3);
    assert_eq!(
        offline["outbox_depth"], 41,
        "and its own publish backlog — the opposite direction, and the one that was invisible"
    );
    // The split, which is the whole point of reporting the held generation (ADR-0108): without both
    // numbers this box is indistinguishable from one that is merely quiet.
    assert_eq!(offline["lease_generation_held"], 2);
    assert_eq!(offline["lease_generation_authoritative"], 3);
    assert_eq!(
        offline["lease_superseded"], true,
        "behind its store's authority: replaced, and refusing updates because of it"
    );
    assert_eq!(
        offline["outbox_reported_at_ms"],
        seen_ago(600_000).as_milliseconds_since_epoch(),
        "the depth travels with the heartbeat that reported it, so a stale one reads as stale"
    );
    assert_eq!(offline["relay_oldest_pending_at_ms"], NOW_MS - 120_000);
}

#[tokio::test]
async fn fleet_never_seen_store_is_offline_and_not_current() {
    let store = StoreId::new(Ulid::from_u128(0x00F1_EE7C));
    let fleet = FakeFleet::default().with_row(
        tenant(),
        FleetRow {
            store_id: store,
            name: "Phú Mỹ Hưng".to_owned(),
            status: EntityStatus::Active,
            last_seen_at: None,
            last_config_pull_at: None,
            config_version_held: None,
            config_version_published: Some("v-current".to_owned()),
            relay_backlog: 0,
            relay_oldest_pending_at: None,
            installed_version: None,
            self_test_ok: None,
            reported_at: None,
            outbox_depth: None,
            outbox_reported_at: None,
            lease_generation_held: None,
            lease_reported_at: None,
            lease_generation_authoritative: None,
            edge_placement: StorePlacement::NeverIssued,
            region: None,
            superseded_generation: None,
            retired_at: None,
            retired_by: None,
            print_agents: None,
            print_agents_reported_at: None,
        },
    );
    let router = fleet_app(provisioned_admin(), fleet);
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();

    let listed = router
        .oneshot(get_with_cookie(
            &format!("/admin/fleet?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the fleet list");
    let row = &json_body(listed).await[0];
    assert_eq!(
        row["online"], false,
        "a store that never checked in is offline"
    );
    assert_eq!(
        row["last_seen_at_ms"],
        serde_json::Value::Null,
        "and carries no last-seen instant"
    );
    assert_eq!(
        row["config_current"], false,
        "a store holding nothing is not current, even against a published version"
    );
}

#[tokio::test]
async fn fleet_reads_one_store_and_404s_an_unknown_one() {
    let store = StoreId::new(Ulid::from_u128(0x00F1_EE7D));
    let fleet = FakeFleet::default().with_row(
        tenant(),
        FleetRow {
            store_id: store,
            name: "Thảo Điền".to_owned(),
            status: EntityStatus::Active,
            last_seen_at: Some(seen_ago(1_000)),
            last_config_pull_at: Some(seen_ago(1_000)),
            config_version_held: Some("v-current".to_owned()),
            config_version_published: Some("v-current".to_owned()),
            relay_backlog: 0,
            relay_oldest_pending_at: None,
            installed_version: None,
            self_test_ok: None,
            reported_at: None,
            outbox_depth: None,
            outbox_reported_at: None,
            lease_generation_held: None,
            lease_reported_at: None,
            lease_generation_authoritative: None,
            edge_placement: StorePlacement::NeverIssued,
            region: None,
            superseded_generation: None,
            retired_at: None,
            retired_by: None,
            print_agents: None,
            print_agents_reported_at: None,
        },
    );
    let router = fleet_app(provisioned_admin(), fleet);
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store.as_ulid().to_string();

    let found = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/fleet/{store_ulid}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the store detail");
    assert_eq!(found.status(), StatusCode::OK);
    assert_eq!(json_body(found).await["name"], "Thảo Điền");

    let unknown = StoreId::new(Ulid::from_u128(0xDEAD)).as_ulid().to_string();
    let missing = router
        .oneshot(get_with_cookie(
            &format!("/admin/fleet/{unknown}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the store detail");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn fleet_needs_a_session() {
    let router = fleet_app(provisioned_admin(), FakeFleet::default());
    let tenant_ulid = tenant().as_ulid().to_string();
    let denied = router
        .oneshot(get(&format!("/admin/fleet?tenant_id={tenant_ulid}"), None))
        .await
        .expect("route the fleet list");
    assert_eq!(
        denied.status(),
        StatusCode::UNAUTHORIZED,
        "the fleet view is behind the admin session guard"
    );
}
