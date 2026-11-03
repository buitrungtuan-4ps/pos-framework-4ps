//! SPIKE (throwaway, never merged): publish -> edge-apply latency for a config wake on a per-store
//! NATS subject (ADR-0195), over a real local nats-server.
//!
//! Run: `NATS_URL=nats://127.0.0.1:4222 cargo test -p pos-edge --release --test spike_wake -- --ignored --nocapture`
//!
//! The "cloud" publishes `pos.store.<id>.config` on one client. The "edge" subscribes on another
//! client, and on each wake it runs the real `ConfigClient::pump_once` against a simulated cloud
//! transport. That transport sleeps a configurable round trip and parses the document from a JSON
//! string. The edge is a real `Edge` over the in-memory store. A till subscribed to the fan-out
//! stamps the `config_applied` frame.
#![allow(clippy::disallowed_methods, clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use pos_edge::config_client::{ConfigClient, ConfigTransport, ConfigTransportError, SyncedConfig};
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::FakeStore;
use pos_proto::SalesChannel;
use pos_proto::ids::{MenuItemId, StoreId, TaxClassId};
use pos_proto::menu::{MenuBook, MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;

struct Cloud {
    version: Mutex<u128>,
    document: String,
    rtt: Duration,
}

struct SimulatedTransport(Arc<Cloud>);

impl ConfigTransport for SimulatedTransport {
    async fn fetch(&self, held: Option<&str>) -> Result<Option<SyncedConfig>, ConfigTransportError> {
        tokio::time::sleep(self.0.rtt).await;
        let version = Ulid::from_u128(*self.0.version.lock().unwrap()).to_string();
        if held == Some(version.as_str()) {
            return Ok(None);
        }
        let document = serde_json::from_str(&self.0.document).map_err(|e| ConfigTransportError::new(e.to_string()))?;
        Ok(Some(SyncedConfig { config_version_id: version, document }))
    }
}

/// A fake menu of `n` items (obviously fake names), as JSON.
fn menu_document(n: u128) -> String {
    let mut catalog = MenuCatalog::new();
    for i in 0..n {
        catalog = catalog.with(MenuEntry::new(
            MenuItemId::new(Ulid::from_u128(1000 + i)),
            DisplayName::new(format!("Demo item {i}")),
            Money::new(CurrencyCode::VND, 50_000 + (i as i64) * 1_000),
            TaxClassId::new(Ulid::from_u128(1)),
        ));
    }
    let book = MenuBook::new().with(SalesChannel::DineIn, catalog);
    serde_json::json!({ "menu": book }).to_string()
}

fn pct(sorted: &[f64], q: f64) -> f64 {
    sorted[((q * sorted.len() as f64).ceil() as usize).max(1) - 1]
}

async fn run(url: &str, items: u128, rtt_ms: u64, samples: usize) {
    let store_id = StoreId::new(Ulid::from_u128(7));
    let cloud = Arc::new(Cloud { version: Mutex::new(1), document: menu_document(items), rtt: Duration::from_millis(rtt_ms) });
    let edge = Arc::new(
        Edge::new(FakeStore::default(), StoreIdentity::for_store(store_id), EdgeSession::bootstrap(), Arc::new(InMemoryReceipts::new()))
            .unwrap(),
    );
    let mut till = edge.fanout().subscribe();
    let client = Arc::new(ConfigClient::new(SimulatedTransport(cloud.clone()), edge.clone(), None));
    client.pump_once().await.unwrap(); // the store starts current

    let subject = format!("pos.store.{store_id}.config");
    let edge_nats = async_nats::connect(url).await.unwrap();
    let mut wakes = edge_nats.subscribe(subject.clone()).await.unwrap();
    edge_nats.flush().await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Instant, Instant)>(4);
    let pump = client.clone();
    tokio::spawn(async move {
        while let Some(_wake) = wakes.next().await {
            let received = Instant::now();
            pump.pump_once().await.unwrap();
            let applied = Instant::now();
            tx.send((received, applied)).await.unwrap();
        }
    });

    let cloud_nats = async_nats::connect(url).await.unwrap();
    let (mut wake, mut pull, mut total, mut till_ms) = (vec![], vec![], vec![], vec![]);
    for k in 0..samples {
        *cloud.version.lock().unwrap() = 2 + k as u128;
        while till.try_recv().is_ok() {}
        let published = Instant::now();
        let payload = format!("{{\"config_version_id\":\"{}\"}}", Ulid::from_u128(2 + k as u128));
        cloud_nats.publish(subject.clone(), payload.into()).await.unwrap();
        cloud_nats.flush().await.unwrap();
        let (received, applied) = rx.recv().await.unwrap();
        let frame = till.recv().await.unwrap();
        let framed = Instant::now();
        assert!(frame.contains("config_applied"));
        let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1000.0;
        wake.push(ms(published, received));
        pull.push(ms(received, applied));
        total.push(ms(published, applied));
        till_ms.push(ms(published, framed));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    for v in [&mut wake, &mut pull, &mut total, &mut till_ms] {
        v.sort_by(f64::total_cmp);
    }
    println!(
        "| {items} items ({} KB) | {rtt_ms} ms | {samples} | {:.2} / {:.2} | {:.2} / {:.2} | {:.2} / {:.2} | {:.2} / {:.2} |",
        cloud.document.len() / 1024,
        pct(&wake, 0.5), pct(&wake, 0.95),
        pct(&pull, 0.5), pct(&pull, 0.95),
        pct(&total, 0.5), pct(&total, 0.95),
        pct(&till_ms, 0.5), pct(&till_ms, 0.95),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "spike: needs NATS_URL and a running nats-server"]
async fn publish_to_apply_latency() {
    let url = std::env::var("NATS_URL").expect("NATS_URL");
    println!("| document | simulated cloud RTT | n | wake p50 / p95 ms | pull+apply p50 / p95 | publish→applied p50 / p95 | publish→till frame p50 / p95 |");
    println!("|---|---|---|---|---|---|---|");
    for (items, rtt) in [(1, 0), (300, 0), (1000, 0), (300, 40), (300, 120)] {
        run(&url, items, rtt, 200).await;
    }
}
