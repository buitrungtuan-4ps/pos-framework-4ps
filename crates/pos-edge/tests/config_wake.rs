// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The config loop pulls when the cloud wakes the store
//! ([ADR-0195](../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md) decision 2): at
//! once for a version it does not hold, never for the one it holds, once when the bus connects, at
//! most once a second however many wakes arrive, and on the poll whatever the bus does.
//!
//! On the real clock with a two-second poll, so each case runs in a few seconds.

use std::sync::Mutex;

use pos_edge::app::ConfigDelivery;
use pos_edge::config_client::{
    ConfigClient, ConfigTransport, ConfigTransportError, SyncedConfig, WAKE_PULL_SPACING,
};
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::FakeStore;
use pos_ports::PortError;
use pos_ports::store_wake::{StoreWake, WakeSignal};
use pos_proto::ids::{ConfigVersionId, StoreId};
use pos_proto::ulid::Ulid;
use pos_proto::wake::ConfigWake;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, Instant, sleep};

const POLL: Duration = Duration::from_secs(2);

fn version(n: u128) -> String {
    Ulid::from_u128(n).to_string()
}

fn wake_for(n: u128) -> WakeSignal {
    WakeSignal::Wake(Some(ConfigWake {
        config_version_id: ConfigVersionId::new(Ulid::from_u128(n)),
    }))
}

/// A cloud holding one current version; every pull is timed.
struct Cloud {
    current: Mutex<String>,
    pulls: Mutex<Vec<Instant>>,
}

impl Cloud {
    fn at(n: u128) -> Self {
        Self {
            current: Mutex::new(version(n)),
            pulls: Mutex::new(Vec::new()),
        }
    }

    fn publish(&self, n: u128) {
        *self.current.lock().expect("current") = version(n);
    }

    fn pulls(&self) -> usize {
        self.pulls.lock().expect("pulls").len()
    }
}

impl ConfigTransport for &Cloud {
    async fn fetch(
        &self,
        held_version: Option<&str>,
    ) -> Result<Option<SyncedConfig>, ConfigTransportError> {
        self.pulls.lock().expect("pulls").push(Instant::now());
        let current = self.current.lock().expect("current").clone();
        Ok(
            (held_version != Some(current.as_str())).then(|| SyncedConfig {
                config_version_id: current,
                document: serde_json::json!({}),
            }),
        )
    }
}

/// A wake fed by the test, keeping the port's contract: a wait ends at `within` with nothing.
struct TestWake(tokio::sync::Mutex<mpsc::Receiver<WakeSignal>>);

impl StoreWake for TestWake {
    async fn next_signal(&self, within: Duration) -> Result<Option<WakeSignal>, PortError> {
        let mut receiver = self.0.lock().await;
        match tokio::time::timeout(within, receiver.recv()).await {
            Ok(Some(signal)) => Ok(Some(signal)),
            Ok(None) => Err(PortError::unavailable(
                pos_ports::PortName::StoreWake,
                "closed",
            )),
            Err(_) => Ok(None),
        }
    }
}

fn edge() -> Arc<Edge<FakeStore>> {
    Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(StoreId::new(Ulid::from_u128(7))),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("an edge"),
    )
}

/// Runs the loop over `cloud` while `script` drives it with the wake sender, then stops it.
async fn with_loop<F>(
    cloud: &Cloud,
    script: impl FnOnce(mpsc::Sender<WakeSignal>, Arc<Edge<FakeStore>>) -> F,
) where
    F: Future<Output = ()>,
{
    let (sender, receiver) = mpsc::channel(256);
    let (stop, stopped) = oneshot::channel::<()>();
    let edge = edge();
    let client = ConfigClient::new(cloud, Arc::clone(&edge), None);
    let run = client.run_waking(
        TestWake(tokio::sync::Mutex::new(receiver)),
        POLL,
        async move {
            let _ = stopped.await;
        },
    );
    let drive = async move {
        script(sender, edge).await;
        let _ = stop.send(());
    };
    tokio::join!(run, drive);
}

#[tokio::test]
async fn a_wake_pulls_at_once_and_a_wake_for_the_held_version_does_nothing() {
    let cloud = &Cloud::at(1);
    with_loop(cloud, |wake, edge| async move {
        sleep(Duration::from_millis(100)).await;
        assert_eq!(cloud.pulls(), 1, "the first pull, on starting");

        cloud.publish(2);
        wake.send(wake_for(2)).await.expect("send");
        sleep(Duration::from_millis(100)).await;
        assert_eq!(cloud.pulls(), 2, "a wake for a new version pulls at once");
        let report = edge.config_wake_standing().report();
        assert_eq!(report.samples, 1, "its wake-to-apply time is kept");
        assert!(report.max_ms.is_some_and(|ms| ms < 100));

        wake.send(wake_for(2)).await.expect("send");
        sleep(Duration::from_millis(500)).await;
        assert_eq!(
            cloud.pulls(),
            2,
            "a wake for the held version pulls nothing"
        );

        sleep(Duration::from_millis(1700)).await;
        assert_eq!(cloud.pulls(), 3, "and the poll carries on");
    })
    .await;
}

#[tokio::test]
async fn the_bus_connecting_pulls_and_a_flood_of_wakes_pulls_at_most_once_a_second() {
    let cloud = &Cloud::at(1);
    with_loop(cloud, |wake, edge| async move {
        sleep(Duration::from_millis(100)).await;
        assert_eq!(
            edge.config_wake_standing().report().delivery,
            ConfigDelivery::Poll,
            "on the poll until the bus says it is connected"
        );
        wake.send(WakeSignal::Connected).await.expect("send");
        sleep(Duration::from_millis(100)).await;
        assert_eq!(cloud.pulls(), 2, "a (re)connect pulls once, to catch up");
        assert_eq!(
            edge.config_wake_standing().report().delivery,
            ConfigDelivery::Wake
        );

        let flood_start = Instant::now();
        for n in 10..110 {
            cloud.publish(n);
            wake.send(wake_for(n)).await.expect("send");
        }
        sleep(Duration::from_millis(1800)).await;
        let pulls = cloud.pulls.lock().expect("pulls").clone();
        let during: Vec<_> = pulls.iter().filter(|at| **at >= flood_start).collect();
        assert!(
            !during.is_empty() && during.len() <= 2,
            "a hundred wakes collapse into a pull or two: {}",
            during.len()
        );
        for pair in pulls.windows(2).skip(1) {
            let [earlier, later] = pair else { continue };
            assert!(
                *later - *earlier >= WAKE_PULL_SPACING,
                "wake-driven pulls are a second apart"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn a_dead_bus_leaves_the_poll() {
    let cloud = &Cloud::at(1);
    with_loop(cloud, |wake, edge| async move {
        wake.send(WakeSignal::Connected).await.expect("send");
        sleep(Duration::from_millis(100)).await;
        drop(wake);
        sleep(Duration::from_millis(100)).await;
        assert_eq!(cloud.pulls(), 2, "the start and the connect");
        assert_eq!(
            edge.config_wake_standing().report().delivery,
            ConfigDelivery::Poll,
            "a bus that fails puts the store back on the poll"
        );
        cloud.publish(2);
        sleep(POLL + Duration::from_millis(300)).await;
        assert_eq!(cloud.pulls(), 3, "the poll still delivers");
    })
    .await;
}
