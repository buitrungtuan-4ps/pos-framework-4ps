// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge store: one SQLite database implementing both `EventStore` and `ConfigStore`.
//!
//! [ADR-0015](../../../docs/adr/0015-sqlite-access.md) and
//! [ADR-0026](../../../docs/adr/0026-port-shapes.md). SQLite is synchronous and single-writer;
//! the ports are async. [`SqliteStore`] bridges the two with one dedicated writer thread that owns
//! the connection: the async methods send a command over a channel and await a reply, so the
//! blocking C calls never touch the async executor and every write serialises through one point.
//! That single point is what makes the outbox position — a monotone rowid assigned inside the commit
//! transaction — gapless without any cross-task coordination.
//!
//! One type implements both ports, so it has exactly one transaction type ([ADR-0026](../../../docs/adr/0026-port-shapes.md)
//! §2): a config change and the event recording it commit together, or not at all.
//!
//! # Durability
//!
//! The database is WAL with `synchronous = NORMAL`. A crash between `BEGIN IMMEDIATE` and `COMMIT`
//! loses only the uncommitted transaction — the pending writes were buffered in the [`SqliteTx`] and
//! never reached SQLite. That is the exact behaviour the `EventStore` contract suite drives, run
//! against this adapter unchanged.

mod migrations;
mod store;
mod tx;
mod writer;

pub use store::SqliteStore;
pub use tx::SqliteTx;
pub use writer::{
    ClaimedPrintJob, OUTBOX_CAPACITY, PrintAgentBacklog, PrintAgentClaim, PrintAgentStanding,
    PrintEnqueue, QueuedPrintJob,
};
