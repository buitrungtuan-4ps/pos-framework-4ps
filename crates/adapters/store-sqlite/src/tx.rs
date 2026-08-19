// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The transaction handle (ADR-0026 §2).
//!
//! Pending writes live here, not in the store — the same arrangement as the fake, and the reason
//! power loss is a real guarantee rather than a hope. `append` and `apply` buffer into a
//! [`SqliteTx`]; `commit` sends the whole buffer to the writer thread to flush in one SQLite
//! transaction; `rollback` and drop discard it, having written nothing. Because `commit` and
//! `rollback` take `self`, a finished transaction cannot be reused, and because the writes are not
//! sent until `commit`, an event written outside a transaction is unrepresentable.

use tokio::sync::{mpsc, oneshot};

use pos_ports::config_store::ConfigUpdate;
use pos_ports::{PortError, PortName, TxContext};
use pos_proto::envelope::{EventEnvelope, RawPayload};

use crate::writer::Command;

/// A store transaction: a buffer of pending writes and the handle that flushes them.
///
/// Not `Clone`: two handles onto one transaction would let a caller commit it twice, which
/// `commit(self)` exists to prevent.
#[derive(Debug)]
pub struct SqliteTx {
    pub(crate) commands: mpsc::Sender<Command>,
    pub(crate) events: Vec<EventEnvelope<RawPayload>>,
    pub(crate) config: Option<ConfigUpdate>,
}

impl TxContext for SqliteTx {
    async fn commit(self) -> Result<(), PortError> {
        // Nothing to do if the transaction touched nothing — a begin with no append and no apply.
        if self.events.is_empty() && self.config.is_none() {
            return Ok(());
        }
        let (reply, outcome) = oneshot::channel();
        self.commands
            .send(Command::Commit {
                events: self.events,
                config: self.config,
                reply,
            })
            .await
            .map_err(|_| writer_gone())?;
        outcome.await.map_err(|_| writer_gone())?
    }

    async fn rollback(self) -> Result<(), PortError> {
        // The pending writes live in `self`; dropping it is the rollback, so there is nothing to
        // undo. Stated rather than left implicit, because "rollback is a no-op" reads like a bug
        // until you know the writes were never sent.
        Ok(())
    }
}

/// The writer thread is gone — the store was dropped while a transaction was still in flight.
fn writer_gone() -> PortError {
    PortError::unavailable(PortName::EventStore, "the store writer thread is gone")
}
