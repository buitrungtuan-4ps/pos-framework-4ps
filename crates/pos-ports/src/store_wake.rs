// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud → store wake: a store learns that its configuration changed, and pulls
//! ([ADR-0195](../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md)).
//!
//! # Receive only, and never a second delivery path
//!
//! [`crate::MessageLink`] stays outbound only ([ADR-0062](../../../docs/adr/0062-the-relay-wake.md)
//! decision 2). This port is its own, the twenty-first, and it carries a signal rather than data:
//! a [`ConfigWake`] names a version and nothing else, and the store answers it with the same pull
//! it runs every 30 s. Configuration reaches a store one way, through that pull, with its tenant
//! check and last-known-good apply.
//!
//! # Losing a wake is allowed
//!
//! Nothing here is durable. A wake sent while the store was away is caught up by
//! [`WakeSignal::Connected`], which an adapter reports on its first connection and every reconnect,
//! and the poll covers whatever else is lost. So the port has no acknowledgement and no cursor.

use core::future::Future;
use core::time::Duration;

use pos_proto::wake::ConfigWake;

use crate::error::PortError;

/// What the store heard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WakeSignal {
    /// The link connected, for the first time or again: pull once, to catch up on any wake sent
    /// while it was down.
    Connected,
    /// A publish changed this store's configuration. `None` when the message was not a wake the
    /// store could read; that still pulls, because a pull that finds nothing new is cheap.
    Wake(Option<ConfigWake>),
}

/// Hears the wakes the cloud sends one store.
///
/// # Contract
///
/// 1. **Connected comes first.** The first signal after subscribing is [`WakeSignal::Connected`],
///    and every reconnect reports it again, so a store that was away pulls once on return.
/// 2. **Only this store's wakes.** A wake sent to another store is never reported here.
/// 3. **A wake keeps its version.** The [`ConfigWake`] reported is the one that was sent.
/// 4. **Waiting is bounded.** [`Self::next_signal`] returns `Ok(None)` once `within` passes with
///    nothing heard; it never waits longer, so the caller's poll keeps its schedule.
/// 5. **A dead bus is an error, never a hang.** With no connection the call returns
///    [`PortError::unavailable`] or `Ok(None)`, and the store keeps the poll.
pub trait StoreWake: Send + Sync {
    /// The next signal, or `None` when `within` passes with nothing heard.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the bus is unreachable.
    fn next_signal(
        &self,
        within: Duration,
    ) -> impl Future<Output = Result<Option<WakeSignal>, PortError>> + Send;
}
