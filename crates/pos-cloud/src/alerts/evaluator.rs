// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The alert evaluator background loop ([ADR-0073](../../../docs/adr/0073-alerting.md), Track O2).
//!
//! One more background loop, in the mould of the rollup projector and webhook dispatcher: each tick it
//! gathers a read-model snapshot (per-tenant fleet rows and disabled webhook endpoints, the
//! background-loop health), runs the pure [`evaluate`], and [`reconcile`]s the firing set against the
//! [`AlertStore`] — opening new alerts, refreshing live ones, and resolving those that have cleared —
//! then records its own `task_health` tick, so the watcher is itself watched.
//!
//! The store→cloud JetStream capacity condition is supported by [`evaluate`] but not yet fed here: a
//! cloud-side stream-info probe is a flagged follow-up (ADR-0073), so this loop passes `None` for the
//! capacity reading and the other conditions fire live.

use core::future::Future;
use core::time::Duration;

use pos_proto::determinism::ClockSource;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;

use super::channel::AlertChannel;
use super::eval::{AlertThresholds, TenantAlertInput, WebhookRef, evaluate};
use super::model::FiringAlert;
use super::store::{AlertRecord, AlertStore, AlertStoreError};
use crate::fleet::FleetStore;
use crate::health::{ALERT_EVALUATOR, TaskHealthStore, tick_detail};
use crate::registry::RegistryStore;
use crate::webhook::store::WebhookEndpointStore;

/// What one evaluation pass did, for the tick's health detail.
///
/// No longer `Copy`: `delivery_error` carries the reason a push failed, and a reason is a sentence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PassSummary {
    /// How many conditions were firing this pass.
    pub firing: usize,
    /// How many were newly opened (not previously active) — the set delivery acts on.
    pub opened: usize,
    /// How many previously-active alerts were resolved because their condition cleared.
    pub resolved: usize,
    /// Why off-console delivery did not happen, or `None` if it did (or if nothing needed pushing,
    /// or no channel is configured).
    ///
    /// A failed delivery is deliberately *not* a failed pass: the alerts are already stored and
    /// already in the console before a channel is asked, so the evaluator did its job
    /// ([ADR-0073](../../../docs/adr/0073-alerting.md)). It surfaces here so the tick's health detail
    /// can say "firing, and not arriving", which is a different fault from either half being down.
    pub delivery_error: Option<String>,
}

/// Reconciles the firing set against the store: opens a new alert for each firing condition that has
/// no active alert, refreshes the ones that do, and resolves every active alert whose condition is no
/// longer firing. Returns the newly-opened alerts (what delivery acts on) and the resolved count.
///
/// `mint` supplies a fresh alert id for each newly-opened alert; injected so the reconcile is testable
/// without entropy. The loop passes a ULID minter.
///
/// # Errors
///
/// [`AlertStoreError`] if the store could not be read or written.
pub async fn reconcile<S, F>(
    alerts: &S,
    now: Timestamp,
    firing: &[FiringAlert],
    mut mint: F,
) -> Result<(Vec<FiringAlert>, usize), AlertStoreError>
where
    S: AlertStore + Sync,
    F: FnMut() -> String,
{
    let active = alerts.list_active().await?;
    let mut opened = Vec::new();
    for condition in firing {
        let existing = active.iter().find(|a| {
            a.tenant_id == condition.tenant_id
                && a.kind == condition.kind
                && a.dedup_key == condition.dedup_key
        });
        let is_new = existing.is_none();
        let id = existing.map_or_else(&mut mint, |a| a.id.clone());
        // On a refresh the store keeps the original id and first_seen_at (its ON CONFLICT DO UPDATE
        // touches only severity/summary/detail/last_seen), so `first_seen_at: now` here is used only
        // when the row is genuinely new.
        let record = AlertRecord {
            id,
            tenant_id: condition.tenant_id,
            kind: condition.kind,
            dedup_key: condition.dedup_key.clone(),
            severity: condition.severity,
            summary: condition.summary.clone(),
            detail: condition.detail.clone(),
            first_seen_at: now,
            last_seen_at: now,
            resolved_at: None,
            acknowledged_at: None,
        };
        alerts.upsert(&record).await?;
        if is_new {
            opened.push(condition.clone());
        }
    }
    let mut resolved = 0;
    for active_alert in &active {
        let still_firing = firing.iter().any(|c| {
            c.tenant_id == active_alert.tenant_id
                && c.kind == active_alert.kind
                && c.dedup_key == active_alert.dedup_key
        });
        if !still_firing {
            alerts.resolve(&active_alert.id, now).await?;
            resolved += 1;
        }
    }
    Ok((opened, resolved))
}

/// A fresh alert id: `now_ms` as the ULID timestamp and OS entropy (mixed with a per-tick `salt` so
/// two alerts opened in the same tick never collide even if the entropy source is briefly down) as the
/// randomness. Mirrors the id-minting the `/admin` routes use.
fn mint_alert_id(now_ms: i64, salt: u128) -> String {
    let mut bytes = [0_u8; 16];
    let entropy = if getrandom::fill(&mut bytes).is_ok() {
        u128::from_le_bytes(bytes)
    } else {
        0
    };
    let ms = u64::try_from(now_ms.max(0)).unwrap_or(0);
    Ulid::from_parts(ms, entropy ^ salt).to_string()
}

/// Gathers each tenant's fleet rows and disabled webhook endpoints for one evaluation pass.
async fn gather<Rg, Fl, Wh>(
    registry: &Rg,
    fleet: &Fl,
    webhooks: &Wh,
) -> Result<Vec<TenantAlertInput>, String>
where
    Rg: RegistryStore + Sync,
    Fl: FleetStore + Sync,
    Wh: WebhookEndpointStore + Sync,
{
    let tenants = registry.list_tenants().await.map_err(|e| e.to_string())?;
    let mut inputs = Vec::with_capacity(tenants.len());
    // The evaluator reads identity only; the version each row was read at is for a writer
    // (ADR-0094), and this loop never writes.
    for tenant in tenants.into_iter().map(|versioned| versioned.record) {
        let fleet_rows = fleet
            .list_fleet(tenant.tenant_id)
            .await
            .map_err(|e| e.to_string())?;
        let hooks = webhooks
            .list_for_tenant(tenant.tenant_id)
            .await
            .map_err(|e| e.to_string())?;
        let disabled_webhooks = hooks
            .into_iter()
            .filter(|hook| hook.disabled)
            .map(|hook| WebhookRef {
                id: hook.id,
                url: hook.url,
            })
            .collect();
        inputs.push(TenantAlertInput {
            tenant_id: tenant.tenant_id,
            fleet: fleet_rows,
            disabled_webhooks,
        });
    }
    Ok(inputs)
}

/// One full pass: gather, evaluate, reconcile. Split out so the loop body stays small and the errors
/// collapse to one string for the tick's health detail.
async fn pass<Rg, Fl, Wh, Th, Al, Ch>(
    registry: &Rg,
    fleet: &Fl,
    webhooks: &Wh,
    task_health: &Th,
    alerts: &Al,
    channel: Option<&Ch>,
    now: Timestamp,
    thresholds: &AlertThresholds,
) -> Result<PassSummary, String>
where
    Rg: RegistryStore + Sync,
    Fl: FleetStore + Sync,
    Wh: WebhookEndpointStore + Sync,
    Th: TaskHealthStore + Sync,
    Al: AlertStore + Sync,
    Ch: AlertChannel + Sync,
{
    let tenants = gather(registry, fleet, webhooks).await?;
    let health = task_health.list_health().await.map_err(|e| e.to_string())?;
    // The JetStream capacity probe is a flagged follow-up (ADR-0073), so no reading is fed yet.
    let firing = evaluate(now, thresholds, &tenants, &health, None);
    let now_ms = now.as_milliseconds_since_epoch();
    let mut salt = 0_u128;
    let (opened, resolved) = reconcile(alerts, now, &firing, || {
        salt += 1;
        mint_alert_id(now_ms, salt)
    })
    .await
    .map_err(|e| e.to_string())?;
    let delivery_error = deliver_opened(channel, now, &opened).await;
    Ok(PassSummary {
        firing: firing.len(),
        opened: opened.len(),
        resolved,
        delivery_error,
    })
}

/// Pushes the newly-opened alerts to `channel`, returning why it did not work — never an error.
///
/// **The return type is the invariant.** `Option<String>` rather than `Result<(), _>` so there is no
/// arrangement of the caller in which a `?` turns a failed push into a failed pass: the alerts are
/// stored and in the console before this is called, and a channel that cannot be reached does not
/// undo that ([ADR-0073](../../../docs/adr/0073-alerting.md)).
///
/// An empty batch never reaches the channel. A channel asked every tick with nothing to say would
/// deliver "all clear" a thousand times a day, which is how the one message that matters comes to be
/// filtered into a folder nobody reads.
async fn deliver_opened<Ch: AlertChannel + Sync>(
    channel: Option<&Ch>,
    now: Timestamp,
    opened: &[FiringAlert],
) -> Option<String> {
    let channel = channel?;
    if opened.is_empty() {
        return None;
    }
    channel
        .deliver(now, opened)
        .await
        .err()
        .map(|error| error.to_string())
}

/// Runs the alert evaluator until `shutdown` resolves. Each tick gathers the read models, evaluates
/// the conditions, reconciles them against the store, and records its own health.
#[expect(
    clippy::too_many_arguments,
    reason = "each store the loop reads is its own seam handle, plus the clock, thresholds, interval \
              and shutdown; bundling them would just be unpacked here, as the sibling loops' runs are"
)]
pub async fn run<Rg, Fl, Wh, Th, Al, Ch, C>(
    registry: Rg,
    fleet: Fl,
    webhooks: Wh,
    task_health: Th,
    alerts: Al,
    channel: Option<Ch>,
    clock: C,
    thresholds: AlertThresholds,
    interval: Duration,
    shutdown: impl Future<Output = ()>,
) where
    Rg: RegistryStore + Sync,
    Fl: FleetStore + Sync,
    Wh: WebhookEndpointStore + Sync,
    Th: TaskHealthStore + Sync,
    Al: AlertStore + Sync,
    Ch: AlertChannel + Sync,
    C: ClockSource,
{
    tokio::pin!(shutdown);
    loop {
        let now = clock.now();
        let detail = match pass(
            &registry,
            &fleet,
            &webhooks,
            &task_health,
            &alerts,
            channel.as_ref(),
            now,
            &thresholds,
        )
        .await
        {
            Ok(summary) => {
                if summary.opened > 0 || summary.resolved > 0 {
                    tracing::info!(
                        firing = summary.firing,
                        opened = summary.opened,
                        resolved = summary.resolved,
                        "alert evaluator pass"
                    );
                }
                // A delivery failure is logged at warn and reported in the detail, but the tick stays
                // healthy: the alerts are stored and the console has them (ADR-0073).
                if let Some(error) = &summary.delivery_error {
                    tracing::warn!(
                        %error,
                        opened = summary.opened,
                        "alerts opened but off-console delivery failed; they are stored and in the \
                         console"
                    );
                }
                tick_detail(
                    true,
                    interval.as_secs(),
                    serde_json::json!({
                        "firing": summary.firing,
                        "opened": summary.opened,
                        "resolved": summary.resolved,
                        "delivery_error": summary.delivery_error,
                    }),
                )
            }
            Err(error) => {
                tracing::error!(%error, "alert evaluation pass failed; will retry");
                tick_detail(false, interval.as_secs(), serde_json::json!({}))
            }
        };
        // Best-effort health telemetry: a failure to record must never crash the loop.
        if let Err(error) = task_health
            .record_tick(ALERT_EVALUATOR, clock.now(), &detail)
            .await
        {
            tracing::warn!(%error, "recording alert-evaluator task health failed");
        }
        tokio::select! {
            biased;
            () = &mut shutdown => {
                tracing::info!("alert evaluator shutting down");
                return;
            }
            () = tokio::time::sleep(interval) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use pos_proto::ids::TenantId;
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;
    use serde_json::json;

    use super::{AlertChannel, deliver_opened, reconcile};
    use crate::alerts::model::{AlertKind, FiringAlert};
    use crate::alerts::store::{AlertRecord, AlertStore, AlertStoreError};

    /// The same in-memory lifecycle fake the store tests use, minimal for the reconcile test.
    #[derive(Default)]
    struct FakeAlerts {
        rows: Mutex<Vec<AlertRecord>>,
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
            _id: &str,
            _acknowledged_at: Timestamp,
        ) -> Result<(), AlertStoreError> {
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

        async fn list_recent(&self, _limit: u32) -> Result<Vec<AlertRecord>, AlertStoreError> {
            Ok(self.rows.lock().expect("lock").clone())
        }
    }

    fn ts(ms: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(ms).expect("valid timestamp")
    }

    fn offline(store: u128, minutes: i64) -> FiringAlert {
        FiringAlert::new(
            AlertKind::StoreOffline,
            Some(TenantId::new(Ulid::from_u128(1))),
            format!("store-{store}"),
            "offline",
            json!({ "minutes_offline": minutes }),
        )
    }

    /// A channel that records what it was asked to send, and can be told to fail.
    #[derive(Default)]
    struct SpyChannel {
        batches: Mutex<Vec<usize>>,
        fail: bool,
    }

    impl AlertChannel for SpyChannel {
        async fn deliver(
            &self,
            _now: Timestamp,
            alerts: &[FiringAlert],
        ) -> Result<(), crate::alerts::channel::ChannelError> {
            self.batches.lock().expect("lock").push(alerts.len());
            if self.fail {
                return Err(crate::alerts::channel::ChannelError::new(
                    "the endpoint refused with 503",
                ));
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_refused_push_is_reported_and_never_raised() {
        // The property the whole slice rests on. `deliver_opened` returns `Option<String>`, so there
        // is no `?` a future edit could add that would turn this into a failed pass — the alerts are
        // already stored and in the console (ADR-0073).
        let channel = SpyChannel {
            fail: true,
            ..SpyChannel::default()
        };
        let reason = deliver_opened(Some(&channel), ts(1_000), &[offline(1, 6)]).await;
        let reason = reason.expect("the failure is reported");
        assert!(
            reason.contains("503"),
            "the reason survives for the tick's health detail: {reason}"
        );
        assert_eq!(
            channel.batches.lock().expect("lock").as_slice(),
            [1],
            "it was asked once, with the one opened alert"
        );
    }

    #[tokio::test]
    async fn an_empty_batch_never_reaches_the_channel() {
        // Most ticks open nothing. Pushing "all clear" every minute is how the one message that
        // matters gets filtered into a folder nobody reads.
        let channel = SpyChannel::default();
        assert!(
            deliver_opened(Some(&channel), ts(1_000), &[])
                .await
                .is_none()
        );
        assert!(
            channel.batches.lock().expect("lock").is_empty(),
            "the channel was not asked at all"
        );
    }

    #[tokio::test]
    async fn the_whole_opened_batch_goes_in_one_call() {
        // Twelve stores dropping off at once is one notification, not twelve — which is why the seam
        // takes a slice rather than an alert.
        let channel = SpyChannel::default();
        let opened = vec![offline(1, 6), offline(2, 7), offline(3, 8)];
        assert!(
            deliver_opened(Some(&channel), ts(1_000), &opened)
                .await
                .is_none(),
            "a successful push reports nothing"
        );
        assert_eq!(
            channel.batches.lock().expect("lock").as_slice(),
            [3],
            "one call carrying all three, not three calls"
        );
    }

    #[tokio::test]
    async fn console_only_alerting_delivers_nothing_and_reports_nothing() {
        // No channel configured is the default posture, and it is not an error.
        let reason = deliver_opened(None::<&SpyChannel>, ts(1_000), &[offline(1, 6)]).await;
        assert!(reason.is_none());
    }

    #[tokio::test]
    async fn reconcile_opens_new_refreshes_existing_and_resolves_gone() {
        let store = FakeAlerts::default();
        let mut counter = 0_u128;
        let mut mint = || {
            counter += 1;
            format!("id-{counter}")
        };

        // First pass: two conditions firing → both opened.
        let firing = vec![offline(1, 6), offline(2, 7)];
        let (opened, resolved) = reconcile(&store, ts(1_000), &firing, &mut mint)
            .await
            .expect("first pass");
        assert_eq!(opened.len(), 2, "both are newly opened");
        assert_eq!(resolved, 0);
        assert_eq!(store.list_active().await.expect("active").len(), 2);

        // Second pass: store 1 still firing (refreshed, not reopened), store 2 gone (resolved), store
        // 3 new (opened).
        let firing = vec![offline(1, 12), offline(3, 5)];
        let (opened, resolved) = reconcile(&store, ts(5_000), &firing, &mut mint)
            .await
            .expect("second pass");
        assert_eq!(opened.len(), 1, "only store 3 is newly opened");
        assert_eq!(opened[0].dedup_key, "store-3");
        assert_eq!(resolved, 1, "store 2 resolved");
        let active = store.list_active().await.expect("active");
        assert_eq!(active.len(), 2, "store 1 (refreshed) and store 3 (new)");
        let store_one = active
            .iter()
            .find(|a| a.dedup_key == "store-1")
            .expect("store 1 still active");
        assert_eq!(
            store_one.first_seen_at,
            ts(1_000),
            "first_seen kept on refresh"
        );
        assert_eq!(store_one.last_seen_at, ts(5_000), "last_seen advanced");
    }
}
