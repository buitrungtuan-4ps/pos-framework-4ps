// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The webhook dispatch background task ([ADR-0032](../../../docs/adr/0032-webhooks.md)).
//!
//! One task drives every enabled endpoint across the fleet. Each tick it loads the enabled
//! registrations ([`WebhookEndpointStore::load_enabled`], fleet-wide as the trusted role), and for
//! each one: **re-vets** the URL so it can only connect to a currently-approved address (closing the
//! DNS-rebinding gap, [`super::ssrf`]), then delivers pages after the cursor with [`deliver_next`]
//! until the endpoint is caught up, suppressed, or fails. A successful delivery's new cursor is
//! persisted so a restart resumes where it left off; an endpoint the breaker auto-disables after a
//! day of failure is persisted disabled and dropped, so the next tick's `load_enabled` no longer
//! returns it.
//!
//! The live [`WebhookEndpoint`]s — with their cursors and **breakers** — are held in memory across
//! ticks ([ADR-0032](../../../docs/adr/0032-webhooks.md): endpoints are runtime state), so the
//! breaker's consecutive-failure and 24-hour windows accumulate across ticks rather than resetting.
//! The database holds only the durable facts: the registration, the cursor, the disabled flag.

use core::future::Future;
use core::time::Duration;
use std::collections::HashMap;
use std::collections::hash_map::Entry;

use pos_ports::event_store::EventStore;
use pos_proto::determinism::ClockSource;
use pos_proto::time::Timestamp;

use super::dispatch::{DeliveryOutcome, WebhookEndpoint, WebhookTransport, deliver_next};
use super::ssrf::vet_blocking;
use super::store::{WebhookEndpointId, WebhookEndpointStore, WebhookStoreError};

/// How many pages one endpoint may deliver in a single tick, so a far-behind endpoint catching up
/// cannot starve the rest of the fleet — it resumes next tick from its persisted cursor.
const MAX_PAGES_PER_TICK: u32 = 20;

/// Runs the webhook dispatch loop on `interval` until `shutdown` resolves, taking `now` from `clock`.
///
/// A sweep error (the store is unreachable) is logged and retried next tick rather than crashing the
/// cloud — webhooks falling a tick behind is a far smaller problem than a cloud that will not start.
pub async fn run<S, W, T, C, H>(
    events: S,
    webhooks: W,
    transport: T,
    clock: C,
    health: H,
    interval: Duration,
    shutdown: impl Future<Output = ()>,
) where
    S: EventStore,
    W: WebhookEndpointStore,
    T: WebhookTransport,
    C: ClockSource,
    H: crate::health::TaskHealthStore,
{
    let mut live: HashMap<WebhookEndpointId, WebhookEndpoint> = HashMap::new();
    tokio::pin!(shutdown);
    loop {
        let ok = match sweep(&events, &webhooks, &transport, clock.now(), &mut live).await {
            Ok(()) => true,
            Err(error) => {
                tracing::error!(%error, "a webhook dispatch sweep failed; will retry next interval");
                false
            }
        };
        let detail = crate::health::tick_detail(
            ok,
            interval.as_secs(),
            serde_json::json!({ "live_endpoints": live.len() }),
        );
        // Best-effort health telemetry: a failure to record must never crash the dispatch loop.
        if let Err(error) = health
            .record_tick(crate::health::WEBHOOK_DISPATCHER, clock.now(), &detail)
            .await
        {
            tracing::warn!(%error, "recording webhook task health failed");
        }
        tokio::select! {
            biased;
            () = &mut shutdown => {
                tracing::info!("webhook dispatch shutting down");
                return;
            }
            () = tokio::time::sleep(interval) => {}
        }
    }
}

/// One dispatch pass over the enabled fleet.
///
/// Reconciles the live endpoint set with the store (adding new registrations, dropping ones no longer
/// enabled), re-vets each destination, and delivers what it can.
async fn sweep<S, W, T>(
    events: &S,
    webhooks: &W,
    transport: &T,
    now: Timestamp,
    live: &mut HashMap<WebhookEndpointId, WebhookEndpoint>,
) -> Result<(), WebhookStoreError>
where
    S: EventStore,
    W: WebhookEndpointStore,
    T: WebhookTransport,
{
    let enabled = webhooks.load_enabled().await?;
    // Drop endpoints no longer enabled (deleted, or auto-disabled on a previous tick).
    live.retain(|id, _| enabled.iter().any(|endpoint| endpoint.id == *id));

    for persisted in enabled {
        // Re-vet before delivering, so a URL that has since repointed at a forbidden address is not
        // delivered to — the endpoint is skipped this tick, its cursor untouched.
        let vetted = match vet_blocking(&persisted.url).await {
            Ok(vetted) => vetted,
            Err(reason) => {
                tracing::warn!(
                    id = %persisted.id,
                    reason = %reason,
                    "skipping a webhook whose URL no longer vets"
                );
                continue;
            }
        };
        let endpoint = match live.entry(persisted.id) {
            Entry::Occupied(slot) => {
                let endpoint = slot.into_mut();
                endpoint.retarget(vetted);
                endpoint
            }
            Entry::Vacant(slot) => slot.insert(WebhookEndpoint::rehydrate(
                persisted.store_id,
                vetted,
                persisted.secret.clone(),
                persisted.cursor,
            )),
        };
        dispatch_one(events, webhooks, transport, now, persisted.id, endpoint).await;
    }
    Ok(())
}

/// Delivers one endpoint's backlog, up to [`MAX_PAGES_PER_TICK`] pages, persisting each cursor advance
/// and any auto-disable.
async fn dispatch_one<S, W, T>(
    events: &S,
    webhooks: &W,
    transport: &T,
    now: Timestamp,
    id: WebhookEndpointId,
    endpoint: &mut WebhookEndpoint,
) where
    S: EventStore,
    W: WebhookEndpointStore,
    T: WebhookTransport,
{
    for _ in 0..MAX_PAGES_PER_TICK {
        let before = endpoint.cursor();
        match deliver_next(events, endpoint, transport, now).await {
            Ok(DeliveryOutcome::Delivered { .. }) => {
                // The delivery happened but the cursor did not persist; stop rather than re-deliver
                // the same page in a tight loop, and retry from the stored cursor next tick.
                if endpoint.cursor() != before
                    && let Some(cursor) = endpoint.cursor()
                    && let Err(error) = webhooks.save_cursor(id, cursor).await
                {
                    tracing::error!(%id, %error, "persisting a webhook cursor failed");
                    return;
                }
                // Otherwise keep going: catch up the rest of the backlog this tick, up to the bound.
            }
            Ok(DeliveryOutcome::Idle | DeliveryOutcome::Suppressed) => return,
            Ok(DeliveryOutcome::Failed) => {
                // A day of continuous failure trips the breaker's auto-disable; persist it so the next
                // tick's load_enabled drops the endpoint until an operator re-enables it.
                if endpoint.is_disabled()
                    && let Err(error) = webhooks.set_disabled(id, true).await
                {
                    tracing::error!(%id, %error, "persisting a webhook auto-disable failed");
                }
                return;
            }
            Err(error) => {
                tracing::error!(%id, %error, "reading the event log for a webhook failed");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{WebhookEndpointId, sweep};

    use pos_contract_tests::fixtures;
    use pos_fakes::FakeStore;
    use pos_ports::event_store::EventStore;
    use pos_ports::{Transactional as _, TxContext as _};
    use pos_proto::ids::{EventId, StoreId, TenantId};
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;

    use crate::webhook::dispatch::{DeliveryError, WebhookTransport};
    use crate::webhook::sign::{Signature, SigningSecret};
    use crate::webhook::ssrf::VettedUrl;
    use crate::webhook::store::{
        PersistedWebhook, WebhookEndpointStore, WebhookStoreError, WebhookSummary,
    };

    fn store_id() -> StoreId {
        StoreId::new(Ulid::from_u128(0x0ADA))
    }

    fn now() -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(1_700_000_000_000).expect("valid")
    }

    async fn seed(store: &FakeStore, count: u32) {
        let events = fixtures::activations(store_id(), 1, count);
        let mut tx = store.begin().await.expect("begin");
        store.append(&mut tx, &events).await.expect("append");
        tx.commit().await.expect("commit");
    }

    /// Records the deliveries it accepts, or refuses them all when down.
    struct FakeTransport {
        deliveries: Mutex<usize>,
        down: AtomicBool,
    }

    impl FakeTransport {
        fn up() -> Self {
            Self {
                deliveries: Mutex::new(0),
                down: AtomicBool::new(false),
            }
        }

        fn count(&self) -> usize {
            *self.deliveries.lock().expect("lock")
        }
    }

    impl WebhookTransport for FakeTransport {
        async fn deliver(
            &self,
            _target: &VettedUrl,
            _signature: &Signature,
            _delivery_id: Option<&str>,
            _body: &[u8],
        ) -> Result<(), DeliveryError> {
            if self.down.load(Ordering::SeqCst) {
                return Err(DeliveryError::new("down"));
            }
            *self.deliveries.lock().expect("lock") += 1;
            Ok(())
        }
    }

    /// A webhook store holding one endpoint, recording the cursor the dispatcher persists.
    #[derive(Default)]
    struct FakeWebhooks {
        rows: Mutex<Vec<PersistedWebhook>>,
    }

    impl FakeWebhooks {
        fn with(endpoint: PersistedWebhook) -> Self {
            Self {
                rows: Mutex::new(vec![endpoint]),
            }
        }

        fn cursor_of(&self, id: WebhookEndpointId) -> Option<EventId> {
            self.rows
                .lock()
                .expect("lock")
                .iter()
                .find(|row| row.id == id)
                .and_then(|row| row.cursor)
        }
    }

    impl WebhookEndpointStore for FakeWebhooks {
        async fn insert(&self, _endpoint: &PersistedWebhook) -> Result<(), WebhookStoreError> {
            Ok(())
        }

        async fn list_for_tenant(
            &self,
            _tenant_id: TenantId,
        ) -> Result<Vec<WebhookSummary>, WebhookStoreError> {
            Ok(Vec::new())
        }

        async fn delete(
            &self,
            _tenant_id: TenantId,
            _id: WebhookEndpointId,
        ) -> Result<bool, WebhookStoreError> {
            Ok(false)
        }

        async fn load_enabled(&self) -> Result<Vec<PersistedWebhook>, WebhookStoreError> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .filter(|row| !row.disabled)
                .cloned()
                .collect())
        }

        async fn save_cursor(
            &self,
            id: WebhookEndpointId,
            cursor: EventId,
        ) -> Result<(), WebhookStoreError> {
            for row in self.rows.lock().expect("lock").iter_mut() {
                if row.id == id {
                    row.cursor = Some(cursor);
                }
            }
            Ok(())
        }

        async fn set_disabled(
            &self,
            id: WebhookEndpointId,
            disabled: bool,
        ) -> Result<(), WebhookStoreError> {
            for row in self.rows.lock().expect("lock").iter_mut() {
                if row.id == id {
                    row.disabled = disabled;
                }
            }
            Ok(())
        }
    }

    /// A persisted endpoint pointing at a public IP literal (so re-vetting needs no DNS).
    fn endpoint(id: u128) -> PersistedWebhook {
        PersistedWebhook {
            id: WebhookEndpointId::new(Ulid::from_u128(id)),
            tenant_id: TenantId::new(Ulid::from_u128(0x7E)),
            store_id: store_id(),
            url: "https://93.184.216.34/hook".to_owned(),
            secret: SigningSecret::new("shhh"),
            cursor: None,
            disabled: false,
        }
    }

    #[tokio::test]
    async fn a_sweep_delivers_the_backlog_and_persists_the_cursor() {
        let events = FakeStore::new();
        seed(&events, 3).await;
        let id = WebhookEndpointId::new(Ulid::from_u128(0xE1));
        let webhooks = FakeWebhooks::with(endpoint(0xE1));
        let transport = FakeTransport::up();
        let mut live = std::collections::HashMap::new();

        sweep(&events, &webhooks, &transport, now(), &mut live)
            .await
            .expect("sweep");

        assert_eq!(transport.count(), 1, "the backlog delivered in one page");
        assert!(
            webhooks.cursor_of(id).is_some(),
            "a successful delivery persisted the new cursor, so a restart resumes from it"
        );
        assert!(
            live.contains_key(&id),
            "the endpoint is held live across ticks"
        );

        // A second sweep with nothing new is idle: no further delivery.
        sweep(&events, &webhooks, &transport, now(), &mut live)
            .await
            .expect("second sweep");
        assert_eq!(transport.count(), 1, "caught up, so nothing more is sent");
    }

    #[tokio::test]
    async fn a_url_that_no_longer_vets_is_skipped_not_delivered() {
        let events = FakeStore::new();
        seed(&events, 2).await;
        let id = WebhookEndpointId::new(Ulid::from_u128(0xE2));
        let mut row = endpoint(0xE2);
        // A URL that re-vets to a forbidden (loopback) address — an IP literal, so still no DNS.
        row.url = "https://127.0.0.1/hook".to_owned();
        let webhooks = FakeWebhooks::with(row);
        let transport = FakeTransport::up();
        let mut live = std::collections::HashMap::new();

        sweep(&events, &webhooks, &transport, now(), &mut live)
            .await
            .expect("sweep");

        assert_eq!(
            transport.count(),
            0,
            "a now-unsafe URL is never delivered to"
        );
        assert!(
            webhooks.cursor_of(id).is_none(),
            "and its cursor is untouched"
        );
        assert!(!live.contains_key(&id), "it is not held live");
    }
}
