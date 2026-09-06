// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The pure alert evaluator ([ADR-0073](../../../docs/adr/0073-alerting.md)).
//!
//! [`evaluate`] turns a read-model snapshot — per-tenant fleet rows and disabled webhook endpoints,
//! the background-loop health rows, and an optional JetStream capacity reading — into the set of
//! [`FiringAlert`]s. It does no I/O and reads no clock: the caller passes `now` and the thresholds, so
//! every condition is exhaustively testable without a database. The loop (slice 3) gathers the
//! snapshot and reconciles the result against the alert store.

use pos_ports::message_link::LinkCapacity;
use pos_proto::ids::TenantId;
use pos_proto::time::Timestamp;
use serde_json::{Value, json};

use super::model::{AlertKind, AlertSeverity, FiringAlert};
use crate::fleet::FleetRow;
use crate::health::{ROLLUP_PROJECTOR, TaskHealth};
use crate::registry::EntityStatus;

/// The tunable bounds each condition fires against. Cloud-wide (not per-tenant); the loop reads them
/// from [`CloudConfig`](crate::config::CloudConfig).
#[derive(Debug, Clone, Copy)]
pub struct AlertThresholds {
    /// Seconds since a store's last check-in beyond which it is "offline" (O2 wants 5 minutes).
    pub store_offline_secs: u64,
    /// Relay backlog count at or above which a store's queue is "backed up" (0 disables the count
    /// trigger, leaving only the age trigger).
    pub relay_backlog_max: u64,
    /// Seconds the oldest still-pending relayed order may sit before the queue is "stuck".
    pub relay_oldest_secs: u64,
    /// Percent-of-capacity at or above which the JetStream is "near capacity".
    pub jetstream_capacity_percent: u32,
    /// Extra seconds past a loop's own interval before its silence counts as "stale".
    pub projector_stale_slack_secs: u64,
}

impl Default for AlertThresholds {
    fn default() -> Self {
        Self {
            store_offline_secs: 300,
            relay_backlog_max: 100,
            relay_oldest_secs: 900,
            jetstream_capacity_percent: 80,
            projector_stale_slack_secs: 60,
        }
    }
}

/// A minimal reference to a disabled webhook endpoint, for the [`AlertKind::WebhookDisabled`] alert.
#[derive(Debug, Clone)]
pub struct WebhookRef {
    /// The endpoint's id (the dedup key).
    pub id: String,
    /// The endpoint's URL (shown to the operator — their own endpoint, not a secret).
    pub url: String,
}

/// One tenant's slice of the snapshot: its fleet rows and its currently-disabled webhook endpoints.
#[derive(Debug, Clone)]
pub struct TenantAlertInput {
    /// The tenant.
    pub tenant_id: TenantId,
    /// Every store's fleet row for the tenant.
    pub fleet: Vec<FleetRow>,
    /// The tenant's webhook endpoints currently flagged disabled.
    pub disabled_webhooks: Vec<WebhookRef>,
}

/// Evaluates every alert condition against the snapshot, returning the set that is firing.
#[must_use]
pub fn evaluate(
    now: Timestamp,
    thresholds: &AlertThresholds,
    tenants: &[TenantAlertInput],
    task_health: &[TaskHealth],
    jetstream: Option<&LinkCapacity>,
) -> Vec<FiringAlert> {
    let now_ms = now.as_milliseconds_since_epoch();
    let mut firing = Vec::new();
    for tenant in tenants {
        for row in &tenant.fleet {
            if row.status != EntityStatus::Active {
                continue;
            }
            firing.extend(store_offline(tenant.tenant_id, row, now_ms, thresholds));
            firing.extend(relay_backlog(tenant.tenant_id, row, now_ms, thresholds));
        }
        for hook in &tenant.disabled_webhooks {
            firing.push(webhook_disabled(tenant.tenant_id, hook));
        }
    }
    firing.extend(projector_unhealthy(task_health, now_ms, thresholds));
    if let Some(capacity) = jetstream {
        firing.extend(jetstream_capacity(capacity, thresholds));
    }
    firing
}

/// Whole seconds to milliseconds, saturating (thresholds are small config values).
fn secs_to_ms(secs: u64) -> i64 {
    i64::try_from(secs).unwrap_or(i64::MAX).saturating_mul(1000)
}

fn store_offline(
    tenant: TenantId,
    row: &FleetRow,
    now_ms: i64,
    thresholds: &AlertThresholds,
) -> Option<FiringAlert> {
    // A store that has never checked in is un-onboarded, not "offline" — only a store that was seen
    // and went quiet fires.
    let seen_ms = row.last_seen_at?.as_milliseconds_since_epoch();
    let idle_ms = now_ms.saturating_sub(seen_ms);
    if idle_ms <= secs_to_ms(thresholds.store_offline_secs) {
        return None;
    }
    let minutes = idle_ms / 60_000;
    // One condition, two urgencies, and the input that separates them is where the store's edge
    // runs (ADR-0110). An in-store edge that has gone quiet is a store we have lost *sight* of: the
    // till is on the counter, ADR-0001's offline guarantee holds, and it is very likely still
    // taking money. A hosted edge that has gone quiet is a store that is not trading at all —
    // there is no machine in the shop to fall back to. Same silence, opposite meaning, so the same
    // rule ends at a different severity rather than at a different kind: an operator filtering by
    // `store_offline` must keep seeing both.
    let may_trade = row.edge_placement.may_trade_offline();
    let severity = if may_trade {
        AlertKind::StoreOffline.default_severity()
    } else {
        AlertSeverity::Critical
    };
    let mut detail = json!({
        "store_id": row.store_id.to_string(),
        "last_seen_at_ms": seen_ms,
        "minutes_offline": minutes,
        // Why this alert is at the severity it is, so the drawer explains itself without the
        // reader having to know the rule.
        "may_be_trading_offline": may_trade,
    });
    if let Some(token) = row.edge_placement.as_wire()
        && let Some(object) = detail.as_object_mut()
    {
        object.insert("edge_placement".to_owned(), json!(token));
    }
    Some(
        FiringAlert::new(
            AlertKind::StoreOffline,
            Some(tenant),
            row.store_id.to_string(),
            format!(
                "Store \u{201c}{}\u{201d} has been offline for {minutes} min",
                row.name
            ),
            detail,
        )
        .at_severity(severity),
    )
}

fn relay_backlog(
    tenant: TenantId,
    row: &FleetRow,
    now_ms: i64,
    thresholds: &AlertThresholds,
) -> Option<FiringAlert> {
    if row.relay_backlog == 0 {
        return None;
    }
    let oldest_age_ms = row.relay_oldest_pending_at.map_or(0, |at| {
        now_ms.saturating_sub(at.as_milliseconds_since_epoch())
    });
    let over_count =
        thresholds.relay_backlog_max > 0 && row.relay_backlog >= thresholds.relay_backlog_max;
    let over_age = oldest_age_ms > secs_to_ms(thresholds.relay_oldest_secs);
    if !over_count && !over_age {
        return None;
    }
    let minutes = oldest_age_ms / 60_000;
    Some(FiringAlert::new(
        AlertKind::RelayBacklog,
        Some(tenant),
        row.store_id.to_string(),
        format!(
            "Store \u{201c}{}\u{201d} has {} orders queued (oldest {minutes} min)",
            row.name, row.relay_backlog
        ),
        json!({
            "store_id": row.store_id.to_string(),
            "backlog": row.relay_backlog,
            "oldest_age_minutes": minutes,
        }),
    ))
}

fn webhook_disabled(tenant: TenantId, hook: &WebhookRef) -> FiringAlert {
    FiringAlert::new(
        AlertKind::WebhookDisabled,
        Some(tenant),
        hook.id.clone(),
        "A webhook endpoint auto-disabled after repeated delivery failures".to_owned(),
        json!({ "endpoint_id": hook.id, "url": hook.url }),
    )
}

fn projector_unhealthy(
    task_health: &[TaskHealth],
    now_ms: i64,
    thresholds: &AlertThresholds,
) -> Option<FiringAlert> {
    // Nothing to judge until the projector has recorded at least one tick (normal at startup).
    let row = task_health.iter().find(|h| h.task == ROLLUP_PROJECTOR)?;
    let last_tick_ms = row.last_tick_at.as_milliseconds_since_epoch();
    let interval_secs = row
        .detail
        .get("interval_secs")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let ok = row
        .detail
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let failed = row
        .detail
        .get("failed")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let stale_after =
        secs_to_ms(interval_secs.saturating_add(thresholds.projector_stale_slack_secs));
    let stale = now_ms.saturating_sub(last_tick_ms) > stale_after;
    let failing = !ok || failed > 0;
    if !stale && !failing {
        return None;
    }
    let reason = if stale { "stale" } else { "failing" };
    Some(FiringAlert::new(
        AlertKind::ProjectorUnhealthy,
        None,
        String::new(),
        format!("The rollup projector is {reason}"),
        json!({
            "reason": reason,
            "last_tick_at_ms": last_tick_ms,
            "ok": ok,
            "failed": failed,
            "interval_secs": interval_secs,
        }),
    ))
}

fn jetstream_capacity(
    capacity: &LinkCapacity,
    thresholds: &AlertThresholds,
) -> Option<FiringAlert> {
    if !capacity.is_at_least(thresholds.jetstream_capacity_percent) {
        return None;
    }
    Some(FiringAlert::new(
        AlertKind::JetstreamCapacity,
        None,
        String::new(),
        format!(
            "The store\u{2192}cloud stream is at or above {}% of capacity",
            thresholds.jetstream_capacity_percent
        ),
        json!({
            "messages": capacity.messages,
            "message_limit": capacity.message_limit,
            "bytes": capacity.bytes,
            "byte_limit": capacity.byte_limit,
            "threshold_percent": thresholds.jetstream_capacity_percent,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::{AlertThresholds, TenantAlertInput, WebhookRef, evaluate};
    use crate::alerts::model::{AlertKind, AlertSeverity};
    use crate::fleet::FleetRow;
    use crate::health::{ROLLUP_PROJECTOR, TaskHealth};
    use crate::lease::StorePlacement;
    use crate::registry::EntityStatus;
    use pos_ports::message_link::LinkCapacity;
    use pos_proto::enums::EdgePlacement;
    use pos_proto::ids::{StoreId, TenantId};
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;
    use serde_json::json;

    fn ts(ms: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(ms).expect("valid timestamp")
    }
    fn tenant_id(n: u128) -> TenantId {
        TenantId::new(Ulid::from_u128(n))
    }
    fn store_id(n: u128) -> StoreId {
        StoreId::new(Ulid::from_u128(n))
    }

    fn store_row(id: u128, name: &str, last_seen_ms: Option<i64>) -> FleetRow {
        FleetRow {
            store_id: store_id(id),
            name: name.to_owned(),
            status: EntityStatus::Active,
            last_seen_at: last_seen_ms.map(ts),
            last_config_pull_at: None,
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
            superseded_generation: None,
        }
    }

    fn one_tenant(fleet: Vec<FleetRow>) -> Vec<TenantAlertInput> {
        vec![TenantAlertInput {
            tenant_id: tenant_id(1),
            fleet,
            disabled_webhooks: Vec::new(),
        }]
    }

    #[test]
    fn a_store_quiet_past_the_threshold_fires_offline_but_a_fresh_one_does_not() {
        let now = ts(10_000_000);
        let thresholds = AlertThresholds::default(); // 300s
        let fleet = vec![
            store_row(10, "Quiet", Some(10_000_000 - 400_000)), // 400s idle → fires
            store_row(11, "Fresh", Some(10_000_000 - 60_000)),  // 60s idle → quiet
        ];
        let firing = evaluate(now, &thresholds, &one_tenant(fleet), &[], None);
        assert_eq!(firing.len(), 1);
        assert_eq!(firing[0].kind, AlertKind::StoreOffline);
        assert_eq!(firing[0].dedup_key, store_id(10).to_string());
        assert_eq!(firing[0].tenant_id, Some(tenant_id(1)));
        assert_eq!(firing[0].severity, AlertSeverity::Warning);
    }

    /// The same silence, three placements, three severities
    /// ([ADR-0110](../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
    ///
    /// The store that has never been bumped keeps the old behaviour, and that is the assertion
    /// protecting every store in the fleet today from a severity change nobody asked for.
    #[test]
    fn a_quiet_store_alerts_by_where_its_edge_runs() {
        let now = ts(10_000_000);
        let thresholds = AlertThresholds::default(); // 300s
        let quiet = 10_000_000 - 400_000; // 400s idle → past the threshold

        let mut in_store = store_row(20, "Bến Thành", Some(quiet));
        in_store.edge_placement = StorePlacement::Known(EdgePlacement::InStore);
        let mut hosted = store_row(21, "Thảo Điền", Some(quiet));
        hosted.edge_placement = StorePlacement::Known(EdgePlacement::HostedByPlatform);
        let never = store_row(22, "Xuân Thủy", Some(quiet)); // NeverIssued, the fleet as it stands

        let firing = evaluate(
            now,
            &thresholds,
            &one_tenant(vec![in_store, hosted, never]),
            &[],
            None,
        );
        let severity_of = |id: u128| {
            firing
                .iter()
                .find(|alert| alert.dedup_key == store_id(id).to_string())
                .map(|alert| alert.severity)
        };

        // A shop-floor box gone quiet: the till still works, so this is "we have lost sight of a
        // store that is probably still selling".
        assert_eq!(severity_of(20), Some(AlertSeverity::Warning));
        // A hosted box gone quiet: there is no machine in the shop. The store is not trading.
        assert_eq!(severity_of(21), Some(AlertSeverity::Critical));
        // No lease row yet — every store in the fleet today. Unchanged behaviour.
        assert_eq!(severity_of(22), Some(AlertSeverity::Warning));

        // All three are still one kind: an operator filtering on `store_offline` sees the lot.
        assert_eq!(firing.len(), 3);
        assert!(
            firing
                .iter()
                .all(|alert| alert.kind == AlertKind::StoreOffline)
        );
    }

    /// A token this build cannot read is scored as the *severe* case, not the safe one.
    ///
    /// This is the failure the three-state [`StorePlacement`] exists to prevent. An
    /// `Option<EdgePlacement>` would make "no row" and "unreadable row" the same `None`, both would
    /// score as in-store, and a hosted store that had gone dark would page one severity too low —
    /// with nothing failing, nothing 500ing, and a plausible-looking alert in the console.
    #[test]
    fn an_unreadable_placement_alerts_at_the_hosted_severity() {
        let now = ts(10_000_000);
        let thresholds = AlertThresholds::default();
        let mut corrupt = store_row(23, "Phú Mỹ Hưng", Some(10_000_000 - 400_000));
        corrupt.edge_placement = StorePlacement::Unreadable;

        let firing = evaluate(now, &thresholds, &one_tenant(vec![corrupt]), &[], None);
        assert_eq!(firing.len(), 1);
        assert_eq!(
            firing[0].severity,
            AlertSeverity::Critical,
            "an unreadable placement must not be scored as though the store were in-store"
        );
    }

    #[test]
    fn a_never_seen_or_archived_store_never_fires_offline() {
        let now = ts(10_000_000);
        let mut archived = store_row(12, "Gone", Some(0));
        archived.status = EntityStatus::Archived;
        let fleet = vec![store_row(13, "NeverSeen", None), archived];
        let firing = evaluate(
            now,
            &AlertThresholds::default(),
            &one_tenant(fleet),
            &[],
            None,
        );
        assert!(firing.is_empty());
    }

    #[test]
    fn a_stuck_relay_queue_fires_by_age_or_count() {
        let now = ts(10_000_000);
        let thresholds = AlertThresholds::default(); // max 100, oldest 900s
        let mut by_age = store_row(20, "OldQueue", Some(10_000_000));
        by_age.relay_backlog = 3;
        by_age.relay_oldest_pending_at = Some(ts(10_000_000 - 1_000_000)); // 1000s → over 900s
        let mut by_count = store_row(21, "BigQueue", Some(10_000_000));
        by_count.relay_backlog = 150; // over 100
        by_count.relay_oldest_pending_at = Some(ts(10_000_000 - 1000));
        let mut fine = store_row(22, "Draining", Some(10_000_000));
        fine.relay_backlog = 2;
        fine.relay_oldest_pending_at = Some(ts(10_000_000 - 1000));
        let firing = evaluate(
            now,
            &thresholds,
            &one_tenant(vec![by_age, by_count, fine]),
            &[],
            None,
        );
        let backlog: Vec<_> = firing
            .iter()
            .filter(|a| a.kind == AlertKind::RelayBacklog)
            .map(|a| a.dedup_key.clone())
            .collect();
        assert_eq!(backlog.len(), 2);
        assert!(backlog.contains(&store_id(20).to_string()));
        assert!(backlog.contains(&store_id(21).to_string()));
    }

    #[test]
    fn a_disabled_webhook_fires_one_alert_per_endpoint() {
        let now = ts(1);
        let tenants = vec![TenantAlertInput {
            tenant_id: tenant_id(1),
            fleet: Vec::new(),
            disabled_webhooks: vec![WebhookRef {
                id: "hook-1".to_owned(),
                url: "https://example.test/hook".to_owned(),
            }],
        }];
        let firing = evaluate(now, &AlertThresholds::default(), &tenants, &[], None);
        assert_eq!(firing.len(), 1);
        assert_eq!(firing[0].kind, AlertKind::WebhookDisabled);
        assert_eq!(firing[0].dedup_key, "hook-1");
    }

    #[test]
    fn the_projector_fires_when_stale_or_failing_but_not_when_healthy() {
        let now = ts(10_000_000);
        let thresholds = AlertThresholds::default(); // slack 60s
        let healthy = TaskHealth {
            task: ROLLUP_PROJECTOR.to_owned(),
            last_tick_at: ts(10_000_000 - 5_000), // 5s ago, within interval+slack
            detail: json!({ "ok": true, "interval_secs": 30, "failed": 0 }),
        };
        assert!(evaluate(now, &thresholds, &[], &[healthy], None).is_empty());

        let failing = TaskHealth {
            task: ROLLUP_PROJECTOR.to_owned(),
            last_tick_at: ts(10_000_000 - 5_000),
            detail: json!({ "ok": true, "interval_secs": 30, "failed": 2 }),
        };
        let firing = evaluate(now, &thresholds, &[], &[failing], None);
        assert_eq!(firing.len(), 1);
        assert_eq!(firing[0].kind, AlertKind::ProjectorUnhealthy);
        assert_eq!(firing[0].tenant_id, None);
        assert_eq!(firing[0].severity, AlertSeverity::Critical);

        let stale = TaskHealth {
            task: ROLLUP_PROJECTOR.to_owned(),
            last_tick_at: ts(10_000_000 - 200_000), // 200s ago, past 30+60
            detail: json!({ "ok": true, "interval_secs": 30, "failed": 0 }),
        };
        assert_eq!(evaluate(now, &thresholds, &[], &[stale], None).len(), 1);
    }

    #[test]
    fn jetstream_fires_only_when_at_or_above_the_threshold() {
        let now = ts(1);
        let thresholds = AlertThresholds::default(); // 80%
        let full = LinkCapacity {
            messages: 90,
            message_limit: Some(100),
            bytes: 0,
            byte_limit: None,
        };
        let roomy = LinkCapacity {
            messages: 10,
            message_limit: Some(100),
            bytes: 0,
            byte_limit: None,
        };
        assert_eq!(
            evaluate(now, &thresholds, &[], &[], Some(&full)).len(),
            1,
            "90/100 crosses 80%"
        );
        assert!(evaluate(now, &thresholds, &[], &[], Some(&roomy)).is_empty());
    }
}
