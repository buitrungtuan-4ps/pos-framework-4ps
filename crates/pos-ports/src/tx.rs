// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The transaction handle, and why it is a supertrait rather than a parameter.
//!
//! `docs/glossary.md` defines the outbox as a queue "written in the same transaction as
//! the state change", and `docs/pos-spec.md` §5 requires the receipt number to be
//! allocated inside the bill transaction. So one transaction spans several operations,
//! and at the edge it spans two ports: `store-sqlite` implements both
//! [`crate::EventStore`] and [`crate::ConfigStore`].
//!
//! [`Transactional`] is what makes that enforceable instead of conventional. Both ports
//! *require* it rather than each declaring its own handle type, so an adapter
//! implementing both has exactly one [`Transactional::Tx`] — and "append the event and
//! allocate the receipt number in one transaction" becomes the only thing that
//! type-checks. Handing `store-postgres`'s handle to `store-sqlite` is a type error, not
//! a review finding.
//!
//! See [ADR-0026](../../../docs/adr/0026-port-shapes.md) §2 for the two rejected
//! alternatives: an ambient implicit transaction, and `&mut dyn TxContext`.

use core::future::Future;

use crate::error::PortError;

/// An open write transaction.
///
/// # Lifecycle
///
/// Obtained only from [`Transactional::begin`]. [`Self::commit`] and [`Self::rollback`]
/// take `self` by value, so a finished transaction cannot be used again — the compiler
/// rejects it rather than the database rejecting it at run time.
///
/// **Dropping without committing rolls back.** SQLite and PostgreSQL both behave this
/// way already; it is stated here as a contract so no adapter can reasonably choose
/// otherwise, and the contract suite checks it.
///
/// # Why `Send` is part of the contract
///
/// Every port declares its futures as `impl Future + Send` so that a binary can hold a
/// port call across a `tokio::spawn`, which `pos_edge`'s request handlers do constantly. A
/// future producing a `Tx` can only be `Send` if `Tx` is, so the bound has to be here
/// rather than left to each binary to discover.
///
/// The shape this rules out is a handle borrowing a thread-local connection. That is not a
/// loss: such a handle cannot be held across an await in a spawned task anyway, so it was
/// never usable in an async server. The direction ADR-0015 is heading — a channel to a
/// dedicated single-writer thread — is `Send` naturally.
#[must_use = "a transaction that is neither committed nor rolled back is a bug, not a no-op"]
pub trait TxContext: Send {
    /// Makes every write in this transaction durable.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be reached, or
    /// [`PortError::internal`] if the commit was rejected. On any error the transaction
    /// is finished and every write in it has been discarded — a failed commit is never
    /// a partial commit.
    fn commit(self) -> impl Future<Output = Result<(), PortError>> + Send;

    /// Discards every write in this transaction.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be reached. A rollback that fails
    /// still leaves nothing committed, so a caller may treat the error as
    /// informational.
    fn rollback(self) -> impl Future<Output = Result<(), PortError>> + Send;
}

/// A port whose writes are transactional.
///
/// Required by [`crate::EventStore`] and [`crate::ConfigStore`] rather than implemented
/// by them, which is the whole point: an adapter that is both has one transaction type,
/// so the two ports share transactions by construction.
pub trait Transactional: Send + Sync {
    /// The handle this adapter hands out.
    ///
    /// Owned rather than borrowed from `&self`. A generic associated lifetime would
    /// model `sqlx` more faithfully, but it fights the single-writer-thread design where
    /// the handle borrows nothing at all, and it costs every signature in the crate its
    /// readability. See ADR-0026 §2.
    type Tx: TxContext;

    /// Opens a write transaction.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be reached, or
    /// [`PortError::resource_exhausted`] if the adapter caps concurrent writers — which
    /// a single-writer SQLite adapter does, and which is back-pressure rather than a
    /// fault.
    fn begin(&self) -> impl Future<Output = Result<Self::Tx, PortError>> + Send;
}
