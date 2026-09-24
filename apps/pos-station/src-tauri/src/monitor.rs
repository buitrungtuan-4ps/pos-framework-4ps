// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The background round: ask the edge how it is, every fifteen seconds, and hand the answer to the
//! station — which updates the tray, raises notifications, opens the till once the edge answers, and
//! returns to the connect page if the pairing is gone.
//!
//! One OS thread with a bounded wake channel, so a successful pairing or a newly opened till is
//! observed at once rather than at the next tick. The HTTP calls are synchronous and run here, off
//! every async task. Which calls are made, and why the signed-in ones wait for an open till, is
//! explained in [`crate::health`].

use std::io;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use tauri::AppHandle;

use crate::address::EdgeOrigin;
use crate::edge::{EdgeClient, EdgeError, Gated};
use crate::health::{
    CloudLink, EdgeReading, NotRead, OutboxLevel, PairingReading, Printer, Snapshot, StoreFacts,
    StoreReading,
};
use crate::station;

/// The round's period: the till's own status bar polls `/api/sync` at the same rate.
const POLL: Duration = Duration::from_secs(15);

/// The period while a till is waiting for its edge to answer, so a terminal switched on before the
/// store PC opens its till within seconds of the edge coming up.
const WAITING_POLL: Duration = Duration::from_secs(3);

/// What one round is asked about, read from the station at the start of the round.
#[derive(Clone)]
pub(crate) struct Target {
    /// The edge, if this device has paired with one.
    pub(crate) origin: Option<EdgeOrigin>,
    /// The device token, if it holds one.
    pub(crate) token: Option<String>,
    /// Whether a till page is open, which is what permits the signed-in reads.
    pub(crate) till_open: bool,
    /// Whether a till is waiting to be opened once the edge answers.
    pub(crate) till_pending: bool,
}

impl core::fmt::Debug for Target {
    /// Redacts the token.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Target")
            .field("origin", &self.origin)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("till_open", &self.till_open)
            .field("till_pending", &self.till_pending)
            .finish()
    }
}

#[derive(Debug)]
enum Wake {
    Now,
    Stop,
}

/// The running monitor.
#[derive(Debug)]
pub(crate) struct Monitor {
    wake: SyncSender<Wake>,
    thread: Option<JoinHandle<()>>,
}

impl Monitor {
    /// Starts the round on its own thread.
    pub(crate) fn start(app: AppHandle) -> io::Result<Self> {
        let (wake, woken) = sync_channel(4);
        let thread = std::thread::Builder::new()
            .name("station-monitor".to_owned())
            .spawn(move || run(&app, &woken))?;
        Ok(Self {
            wake,
            thread: Some(thread),
        })
    }

    /// Runs a round now. A full channel already has a round coming, so it is not an error.
    pub(crate) fn wake(&self) {
        match self.wake.try_send(Wake::Now) {
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => log::warn!("the monitor has stopped"),
        }
    }

    /// Asks the round to stop and does **not** wait: it may be inside an eight-second HTTP timeout,
    /// and quitting should not. The thread ends with the process.
    pub(crate) fn stop(mut self) {
        let _sent = self.wake.try_send(Wake::Stop);
        drop(self.thread.take());
    }
}

fn run(app: &AppHandle, woken: &Receiver<Wake>) {
    let client = EdgeClient::new();
    let mut previous = Snapshot::unknown();
    loop {
        let target = station::target(app);
        let next = observe(&client, &target);
        log::debug!("round: {next:?}");
        let notices = crate::health::notices(&previous, &next);
        station::publish(app, &next, &notices);
        previous = next;
        let period = if target.till_pending {
            WAITING_POLL
        } else {
            POLL
        };
        match woken.recv_timeout(period) {
            Ok(Wake::Now) | Err(RecvTimeoutError::Timeout) => {}
            Ok(Wake::Stop) | Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// One round against the edge.
fn observe(client: &EdgeClient, target: &Target) -> Snapshot {
    let not_read = |reason| StoreReading::NotRead { reason };
    let Some(origin) = target.origin.as_ref() else {
        return Snapshot::unknown();
    };
    let edge = match client.health(origin) {
        Ok(health) => EdgeReading::Up {
            version: health.version,
        },
        Err(error) => {
            log::debug!("{origin}: {error}");
            return Snapshot {
                edge: EdgeReading::Down,
                pairing: PairingReading::Unknown,
                store: not_read(NotRead::EdgeDown),
            };
        }
    };
    let Some(token) = target.token.as_deref() else {
        return Snapshot {
            edge,
            pairing: PairingReading::Unknown,
            store: not_read(NotRead::NotPaired),
        };
    };
    let pairing = match client.still_paired(origin, token) {
        Ok(true) => PairingReading::Paired,
        Ok(false) => PairingReading::Lost,
        Err(error) => {
            log::warn!("{origin}: {error}");
            PairingReading::Unknown
        }
    };
    let store = match pairing {
        PairingReading::Paired if target.till_open => read_store(client, origin, token),
        PairingReading::Paired => not_read(NotRead::TillClosed),
        PairingReading::Lost => not_read(NotRead::NotPaired),
        PairingReading::Unknown => not_read(NotRead::Failed),
    };
    Snapshot {
        edge,
        pairing,
        store,
    }
}

/// The two signed-in reads, only ever made while a till is open. The second is skipped when the
/// first is refused, because it would be refused for the same reason.
fn read_store(client: &EdgeClient, origin: &EdgeOrigin, token: &str) -> StoreReading {
    let facts = settle(origin, client.sync(origin, token)).and_then(|sync| {
        settle(origin, client.printers(origin, token)).map(|printers| StoreFacts {
            cloud_link: CloudLink::from_wire(&sync.cloud_link),
            outbox_depth: sync.outbox_depth,
            outbox_planned_depth: sync.outbox_planned_depth,
            outbox_level: OutboxLevel::from_wire(&sync.outbox_level),
            printers: printers
                .into_iter()
                .map(|printer| Printer {
                    device_id: printer.device_id,
                    name: printer.name,
                })
                .collect(),
        })
    });
    match facts {
        Ok(facts) => StoreReading::Read(facts),
        Err(reason) => StoreReading::NotRead { reason },
    }
}

/// A signed-in read's body, or why there is none.
fn settle<T>(origin: &EdgeOrigin, outcome: Result<Gated<T>, EdgeError>) -> Result<T, NotRead> {
    match outcome {
        Ok(Gated::Read(body)) => Ok(body),
        Ok(Gated::SignInNeeded) => Err(NotRead::SignInNeeded),
        Ok(Gated::NotPaired) => Err(NotRead::NotPaired),
        Err(error) => {
            log::warn!("{origin}: {error}");
            Err(NotRead::Failed)
        }
    }
}
