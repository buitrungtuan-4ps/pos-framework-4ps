// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The write transaction over a pooled connection, and how the pool checks a connection it hands
//! out again — two halves of one rule: no connection returns to the pool with a transaction open.

use deadpool_postgres::{ManagerConfig, Object, RecyclingMethod};

use pos_ports::{PortError, PortName, TxContext};

use crate::store::unavailable;

/// How the pool checks a connection before handing it out again: an empty query, which proves the
/// connection still answers and says nothing.
///
/// It used to be `ROLLBACK`, to end a transaction a dropped [`PgTx`] left open. But every connection
/// came back with none open, so every checkout drew PostgreSQL's `WARNING: there is no transaction
/// in progress`, which `tokio-postgres` logs: 97 % of a pilot's cloud log. A transaction left open
/// now never reaches the pool: [`PgTx`] takes its connection out of the pool instead ([`Drop`]),
/// and closing it ends the transaction.
pub(crate) fn manager_config() -> ManagerConfig {
    ManagerConfig {
        recycling_method: RecyclingMethod::Verified,
    }
}

/// A write transaction over a pooled connection. `begin` issues `BEGIN`; [`TxContext::commit`] and
/// [`TxContext::rollback`] issue `COMMIT`/`ROLLBACK` and return the connection to the pool. Owned,
/// borrowing nothing, so it is `Send` and can be held across a spawn (ADR-0026 §2).
///
/// One dropped before either has succeeded (the crash the contract simulates, a cancelled request,
/// a `COMMIT` that failed) still has its transaction open, so it never goes back to the pool: the
/// drop takes the connection out of the pool and closes it, and the server ends the transaction.
/// The next caller can never run inside it and see its uncommitted rows.
#[derive(Debug)]
pub struct PgTx {
    /// The connection, until `COMMIT` or `ROLLBACK` succeeds and hands it back to the pool.
    connection: Option<Object>,
}

impl PgTx {
    /// A transaction on `connection`, before `BEGIN` is sent, so a drop from then on closes it.
    pub(crate) const fn new(connection: Object) -> Self {
        Self {
            connection: Some(connection),
        }
    }

    /// The connection the transaction runs on.
    pub(crate) fn client(&self) -> Result<&Object, PortError> {
        self.connection.as_ref().ok_or_else(|| {
            PortError::internal(PortName::EventStore, "the transaction has already ended")
        })
    }

    /// Runs `COMMIT` or `ROLLBACK`, and hands the connection back to the pool only if it succeeded.
    async fn end(mut self, statement: &str) -> Result<(), PortError> {
        self.client()?
            .batch_execute(statement)
            .await
            .map_err(unavailable)?;
        // Dropped here with no transaction open, so it goes back to the pool.
        drop(self.connection.take());
        Ok(())
    }
}

impl Drop for PgTx {
    fn drop(&mut self) {
        // Still here means the transaction is still open: out of the pool, so it closes rather
        // than being handed to the next caller.
        if let Some(connection) = self.connection.take() {
            drop(Object::take(connection));
        }
    }
}

impl TxContext for PgTx {
    async fn commit(self) -> Result<(), PortError> {
        self.end("COMMIT").await
    }

    async fn rollback(self) -> Result<(), PortError> {
        self.end("ROLLBACK").await
    }
}
