// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An in-memory [`StoreWake`]
//! ([ADR-0195](../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md)).

use core::time::Duration;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use pos_ports::store_wake::{StoreWake, WakeSignal};
use pos_ports::{PortError, PortName};
use pos_proto::ids::StoreId;
use pos_proto::wake::ConfigWake;

use crate::lock;

/// The most signals a fake holds unheard. A wake beyond it is dropped, as a lost wake may be: the
/// poll delivers the same version.
pub const WAKE_QUEUE: usize = 64;

#[derive(Debug, Default)]
struct WakeState {
    heard: VecDeque<WakeSignal>,
    severed: bool,
}

/// An in-memory `StoreWake` for one store. Its next signal is [`WakeSignal::Connected`], as a new
/// subscription's is; a wait with nothing queued ends at once with nothing.
#[derive(Debug, Clone)]
pub struct FakeStoreWake {
    store_id: StoreId,
    state: Arc<Mutex<WakeState>>,
}

impl FakeStoreWake {
    /// Subscribed for `store_id`, connected and nothing else heard.
    #[must_use]
    pub fn new(store_id: StoreId) -> Self {
        let wake = Self {
            store_id,
            state: Arc::default(),
        };
        wake.push(WakeSignal::Connected);
        wake
    }

    /// The store it hears for.
    #[must_use]
    pub const fn store_id(&self) -> StoreId {
        self.store_id
    }

    /// The cloud woke this store.
    pub fn wake(&self, wake: ConfigWake) {
        self.push(WakeSignal::Wake(Some(wake)));
    }

    /// The connection dropped and came back.
    pub fn reconnect(&self) {
        lock(&self.state).severed = false;
        self.push(WakeSignal::Connected);
    }

    /// The bus is gone: every wait fails as unavailable until [`Self::reconnect`].
    pub fn sever(&self) {
        lock(&self.state).severed = true;
    }

    fn push(&self, signal: WakeSignal) {
        let mut state = lock(&self.state);
        if state.heard.len() < WAKE_QUEUE {
            state.heard.push_back(signal);
        }
    }
}

impl StoreWake for FakeStoreWake {
    async fn next_signal(&self, _within: Duration) -> Result<Option<WakeSignal>, PortError> {
        let mut state = lock(&self.state);
        if state.severed {
            return Err(PortError::unavailable(
                PortName::StoreWake,
                "the bus is unreachable",
            ));
        }
        Ok(state.heard.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use pos_ports::store_wake::{StoreWake, WakeSignal};
    use pos_proto::ids::{ConfigVersionId, StoreId};
    use pos_proto::ulid::Ulid;
    use pos_proto::wake::ConfigWake;

    use super::{FakeStoreWake, WAKE_QUEUE};
    use crate::executor::run_ready;

    #[test]
    fn a_full_queue_drops_the_newest_wake_and_a_severed_bus_is_unavailable() {
        let wake = FakeStoreWake::new(StoreId::new(Ulid::from_u128(1)));
        for n in (0_u128..).take(WAKE_QUEUE + 1) {
            wake.wake(ConfigWake {
                config_version_id: ConfigVersionId::new(Ulid::from_u128(n)),
            });
        }
        let mut heard = 0;
        while run_ready(wake.next_signal(core::time::Duration::ZERO))
            .expect("connected")
            .is_some()
        {
            heard += 1;
        }
        assert_eq!(heard, WAKE_QUEUE, "bounded");

        wake.sever();
        assert!(run_ready(wake.next_signal(core::time::Duration::ZERO)).is_err());
        wake.reconnect();
        assert_eq!(
            run_ready(wake.next_signal(core::time::Duration::ZERO)).expect("back"),
            Some(WakeSignal::Connected)
        );
    }
}
