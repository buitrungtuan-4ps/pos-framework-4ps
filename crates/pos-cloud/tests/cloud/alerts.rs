// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Operational alerts (ADR-0073): the evaluator's tick and its channel, and `/admin/alerts`.

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use pos_cloud::alerts::{AlertKind, AlertRecord, AlertStore, AlertStoreError, ReadModels};
use pos_cloud::audit::AuditSink;
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud::fleet::FleetRow;
use pos_cloud::lease::StorePlacement;
use pos_cloud::registry::{EntityStatus, RegistryStore, TenantRecord};
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeAudit, FakeConfigTrees, FakeFleet, FakeKeys, FakeRegistry, FakeRollups,
    FakeTaskHealth, FakeWebhooks, admin_cookie, app_all, clock, get, get_with_cookie, json_body,
    post_with_cookie, provisioned_admin, seen_ago, tenant,
};

/// An in-memory [`AlertStore`] for the alerts-route tests: the same open→resolved lifecycle the store
/// contract test covers, behind an `Arc<Mutex<…>>` so the router can clone it.
#[derive(Clone, Default)]
pub(crate) struct FakeAlerts {
    rows: Arc<Mutex<Vec<AlertRecord>>>,
}

impl FakeAlerts {
    pub(crate) fn seed(&self, rows: Vec<AlertRecord>) {
        *self.rows.lock().expect("lock") = rows;
    }
}

impl AlertStore for FakeAlerts {
    async fn upsert(&self, record: &AlertRecord) -> Result<(), AlertStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        if let Some(open) = rows
            .iter_mut()
            .find(|r| r.resolved_at.is_none() && r.key() == record.key())
        {
            open.severity = record.severity;
            open.summary.clone_from(&record.summary);
            open.detail = record.detail.clone();
            open.last_seen_at = record.last_seen_at;
        } else {
            rows.push(record.clone());
        }
        Ok(())
    }

    async fn resolve(&self, id: &str, resolved_at: Timestamp) -> Result<(), AlertStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        if let Some(row) = rows
            .iter_mut()
            .find(|r| r.id == id && r.resolved_at.is_none())
        {
            row.resolved_at = Some(resolved_at);
        }
        Ok(())
    }

    async fn acknowledge(
        &self,
        id: &str,
        acknowledged_at: Timestamp,
    ) -> Result<(), AlertStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        if let Some(row) = rows.iter_mut().find(|r| r.id == id) {
            row.acknowledged_at = Some(acknowledged_at);
        }
        Ok(())
    }

    async fn list_active(&self) -> Result<Vec<AlertRecord>, AlertStoreError> {
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .filter(|r| r.resolved_at.is_none())
            .cloned()
            .collect())
    }

    async fn list_recent(&self, limit: u32) -> Result<Vec<AlertRecord>, AlertStoreError> {
        let mut rows = self.rows.lock().expect("lock").clone();
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn list_last_resolved(
        &self,
        kind: AlertKind,
    ) -> Result<Vec<AlertRecord>, AlertStoreError> {
        // The newest resolved row per scope, as the adapter's `DISTINCT ON` returns.
        let mut latest: Vec<AlertRecord> = Vec::new();
        for row in self.rows.lock().expect("lock").iter() {
            if row.kind != kind || row.resolved_at.is_none() {
                continue;
            }
            match latest.iter_mut().find(|kept| kept.key() == row.key()) {
                Some(kept) if kept.resolved_at < row.resolved_at => *kept = row.clone(),
                Some(_) => {}
                None => latest.push(row.clone()),
            }
        }
        Ok(latest)
    }
}

/// An [`AlertChannel`] that records the batches it was handed, so a test can prove the evaluator
/// actually pushes rather than merely counting what it could have pushed.
#[derive(Clone, Default)]
struct SpyAlertChannel {
    batches: Arc<Mutex<Vec<Vec<String>>>>,
}

impl pos_cloud::alerts::AlertChannel for SpyAlertChannel {
    async fn deliver(
        &self,
        _now: Timestamp,
        alerts: &[pos_cloud::alerts::FiringAlert],
    ) -> Result<(), pos_cloud::alerts::ChannelError> {
        self.batches.lock().expect("lock").push(
            alerts
                .iter()
                .map(|alert| format!("{}:{}", alert.kind.as_str(), alert.dedup_key))
                .collect(),
        );
        Ok(())
    }
}

/// The seam is wired: one real evaluator tick over a fleet with an offline store reaches the channel.
///
/// This is the test the slice exists for. The unit tests around `deliver_opened` prove the delivery
/// step behaves; this proves the *loop* calls it, which is the failure mode ADR-0073's "delivery is a
/// separable follow-up" left open and the one nothing else would catch — a `pass` that computed the
/// newly-opened set and dropped it counted `opened: 1` and delivered nothing, and every other test
/// still passed.
#[tokio::test]
async fn one_evaluator_tick_pushes_the_newly_opened_alerts_to_the_channel() {
    let registry = FakeRegistry::default();
    registry
        .create_tenant(&TenantRecord {
            tenant_id: tenant(),
            name: "Pizza 4P's".to_owned(),
            status: EntityStatus::Active,
        })
        .await
        .expect("seed the tenant the evaluator sweeps");
    let silent_store = StoreId::new(Ulid::from_u128(0x0A1E_2733));
    let fleet = FakeFleet::default().with_row(
        tenant(),
        FleetRow {
            store_id: silent_store,
            name: "Thảo Điền".to_owned(),
            status: EntityStatus::Active,
            // Ten minutes of silence, well past the five-minute threshold below.
            last_seen_at: Some(seen_ago(600_000)),
            last_config_pull_at: Some(seen_ago(600_000)),
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
    let channel = SpyAlertChannel::default();

    // `run` passes once on entry and its shutdown select is `biased`, so an already-ready shutdown
    // gives exactly one tick.
    pos_cloud::alerts::evaluator::run(
        ReadModels {
            registry,
            fleet,
            webhooks: FakeWebhooks::default(),
            chain_findings: None,
        },
        FakeTaskHealth::default(),
        FakeAlerts::default(),
        Some(channel.clone()),
        // No stream probe: this exercises the delivery seam, and a deployment with no `[nats]`
        // section passes `None` here exactly as it did before the probe existed (ADR-0087
        // Amendment 2).
        None::<pos_cloud::alerts::probe::CursorProbe>,
        clock(),
        pos_cloud::alerts::AlertThresholds {
            store_offline_secs: 300,
            ..pos_cloud::alerts::AlertThresholds::default()
        },
        core::time::Duration::from_secs(60),
        std::future::ready(()),
    )
    .await;

    let batches = channel.batches.lock().expect("lock").clone();
    assert_eq!(
        batches.len(),
        1,
        "one tick, one batch — not zero (the seam is wired) and not one per alert; got {batches:?}"
    );
    assert_eq!(
        batches[0],
        vec![format!("store_offline:{}", silent_store.as_ulid())],
        "the batch carries the alert the tick opened"
    );
}

/// The same tick with no channel configured is console-only, and that is not an error.
#[tokio::test]
async fn an_evaluator_tick_without_a_channel_still_stores_the_alert() {
    let registry = FakeRegistry::default();
    registry
        .create_tenant(&TenantRecord {
            tenant_id: tenant(),
            name: "Pizza 4P's".to_owned(),
            status: EntityStatus::Active,
        })
        .await
        .expect("seed the tenant");
    let fleet = FakeFleet::default().with_row(
        tenant(),
        FleetRow {
            store_id: StoreId::new(Ulid::from_u128(0x0A1E_2734)),
            name: "Xuân Thủy".to_owned(),
            status: EntityStatus::Active,
            last_seen_at: Some(seen_ago(600_000)),
            last_config_pull_at: Some(seen_ago(600_000)),
            config_version_held: None,
            config_version_published: None,
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
    let alerts = FakeAlerts::default();

    pos_cloud::alerts::evaluator::run(
        ReadModels {
            registry,
            fleet,
            webhooks: FakeWebhooks::default(),
            chain_findings: None,
        },
        FakeTaskHealth::default(),
        alerts.clone(),
        None::<SpyAlertChannel>,
        None::<pos_cloud::alerts::probe::CursorProbe>,
        clock(),
        pos_cloud::alerts::AlertThresholds {
            store_offline_secs: 300,
            ..pos_cloud::alerts::AlertThresholds::default()
        },
        core::time::Duration::from_secs(60),
        std::future::ready(()),
    )
    .await;

    // The in-console channel is the table, and it is the one that always runs (ADR-0073).
    let active = alerts.list_active().await.expect("read the active alerts");
    assert_eq!(
        active.len(),
        1,
        "console-only alerting still opens the alert; only the push is absent"
    );
}

fn alert_at(
    id: &str,
    kind: AlertKind,
    tenant_id: Option<TenantId>,
    dedup: &str,
    resolved: bool,
) -> AlertRecord {
    AlertRecord {
        id: id.to_owned(),
        tenant_id,
        kind,
        dedup_key: dedup.to_owned(),
        severity: kind.default_severity(),
        summary: "a condition".to_owned(),
        detail: serde_json::json!({}),
        first_seen_at: seen_ago(0),
        last_seen_at: seen_ago(0),
        resolved_at: resolved.then(|| seen_ago(0)),
        acknowledged_at: None,
    }
}

/// The main router (for `/admin/login`) merged with the alerts sub-router, one shared admin store.
fn alerts_app(admin: FakeAdmin, alerts: FakeAlerts) -> axum::Router {
    alerts_app_with_audit(admin, alerts, Arc::new(NoopAuditRecorder))
}

/// As [`alerts_app`], recording to a caller-supplied audit trail.
fn alerts_app_with_audit(
    admin: FakeAdmin,
    alerts: FakeAlerts,
    audit: Arc<dyn pos_cloud::audit::AuditRecorder>,
) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::alerts_router(alerts, admin, clock(), audit))
}

#[tokio::test]
async fn alerts_list_active_and_recent_then_acknowledge_and_resolve() {
    let alerts = FakeAlerts::default();
    alerts.seed(vec![
        alert_at("alert-jet", AlertKind::JetstreamCapacity, None, "", false),
        alert_at(
            "alert-store",
            AlertKind::StoreOffline,
            Some(tenant()),
            "store-x",
            false,
        ),
        alert_at(
            "alert-old",
            AlertKind::RelayBacklog,
            Some(tenant()),
            "store-y",
            true,
        ),
    ]);
    let router = alerts_app(provisioned_admin(), alerts);
    let cookie = admin_cookie(&router).await;

    // The active list is the two unresolved alerts, and a server-wide one carries a null tenant.
    let active = router
        .clone()
        .oneshot(get_with_cookie("/admin/alerts", &cookie))
        .await
        .expect("route the active list");
    assert_eq!(active.status(), StatusCode::OK);
    let rows = json_body(active).await;
    let array = rows.as_array().expect("an array of alerts");
    assert_eq!(array.len(), 2, "only the unresolved alerts");
    assert!(
        array
            .iter()
            .any(|a| a["kind"] == "jetstream_capacity" && a["tenant_id"].is_null()),
        "the server-wide alert carries a null tenant"
    );

    // Recent history includes the resolved one.
    let recent = router
        .clone()
        .oneshot(get_with_cookie("/admin/alerts?recent=true", &cookie))
        .await
        .expect("route the recent list");
    let rows = json_body(recent).await;
    assert_eq!(rows.as_array().expect("an array").len(), 3);

    // Acknowledge and resolve are idempotent 204s.
    let ack = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/alerts/alert-store/ack",
            &serde_json::json!({}),
            &cookie,
        ))
        .await
        .expect("route the ack");
    assert_eq!(ack.status(), StatusCode::NO_CONTENT);
    let resolved = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/alerts/alert-store/resolve",
            &serde_json::json!({}),
            &cookie,
        ))
        .await
        .expect("route the resolve");
    assert_eq!(resolved.status(), StatusCode::NO_CONTENT);

    // After the resolve, the active list drops to the server-wide alert alone.
    let active = router
        .oneshot(get_with_cookie("/admin/alerts", &cookie))
        .await
        .expect("route the active list again");
    let rows = json_body(active).await;
    assert_eq!(rows.as_array().expect("an array").len(), 1);
}

#[tokio::test]
async fn alerts_require_a_session() {
    let router = alerts_app(provisioned_admin(), FakeAlerts::default());
    let denied = router
        .oneshot(get("/admin/alerts", None))
        .await
        .expect("route without a cookie");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn acknowledging_a_chain_break_closes_it_and_the_audit_trail_says_who() {
    // A chain break never clears by itself (ADR-0073 amendment 2026-10-31), so a person's
    // acknowledgement is what closes it — and the trail records that it did, with who and when.
    let alerts = FakeAlerts::default();
    alerts.seed(vec![
        alert_at(
            "alert-chain",
            AlertKind::ChainBroken,
            Some(tenant()),
            "store-x",
            false,
        ),
        alert_at(
            "alert-offline",
            AlertKind::StoreOffline,
            Some(tenant()),
            "store-y",
            false,
        ),
    ]);
    let audit = FakeAudit::default();
    let router = alerts_app_with_audit(
        provisioned_admin(),
        alerts.clone(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let cookie = admin_cookie(&router).await;

    for id in ["alert-chain", "alert-offline"] {
        let ack = router
            .clone()
            .oneshot(post_with_cookie(
                &format!("/admin/alerts/{id}/ack"),
                &serde_json::json!({}),
                &cookie,
            ))
            .await
            .expect("route the ack");
        assert_eq!(ack.status(), StatusCode::NO_CONTENT);
    }

    let active = alerts.list_active().await.expect("active");
    assert_eq!(
        active.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
        vec!["alert-offline"],
        "the chain break is closed; an acknowledged offline store still waits for its condition"
    );
    let entries = audit.entries.lock().expect("lock").clone();
    let chain = entries
        .iter()
        .find(|entry| entry.entity_id == "alert-chain")
        .expect("the chain acknowledgement is audited");
    assert_eq!(chain.action, "alert.acknowledge");
    assert!(!chain.actor.admin_id.is_empty(), "who acknowledged it");
    assert_eq!(
        chain
            .after
            .as_ref()
            .and_then(|after| after.get("resolved_at_ms")),
        Some(&serde_json::json!(chain.at.as_milliseconds_since_epoch())),
        "the entry says the acknowledgement resolved it, and when"
    );
    let offline = entries
        .iter()
        .find(|entry| entry.entity_id == "alert-offline")
        .expect("the offline acknowledgement is audited");
    assert!(
        offline.after.is_none(),
        "an ordinary acknowledgement resolves nothing"
    );
}
