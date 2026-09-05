// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The lease generation this box holds, and the standing it derives from it
//! ([ADR-0108](../../../docs/adr/0108-the-lease-generation-is-authority.md), closing
//! [ADR-0049](../../../docs/adr/0049-single-active-lease.md)'s edge half).
//!
//! `pos_core::lease::lease_standing` decides whether this machine is still the store, from two
//! numbers: the **authoritative** generation, which arrives from the cloud in the `lease` config
//! node, and the **held** one, which is this module. Until now neither existed anywhere and the OTA
//! tick passed `LeaseStanding::Active` as a literal — so a box a replacement had superseded went on
//! installing updates as though it were still the store.
//!
//! # Take once, then only compare
//!
//! [`standing`] is the whole rule, and its load-bearing half is easy to get wrong: a box takes its
//! generation **on first sight and never again**. Re-adopt it on each config pull and supersession
//! becomes decorative — a replaced machine reads generation `N + 1`, adopts it, and calls itself
//! active again until the next pull, which is to say forever.
//!
//! The take is therefore an `INSERT … ON CONFLICT DO NOTHING` on the store's own SQLite (migration
//! `0008_lease.sql`), so the rule sits in the schema and not only in the Rust that happens to call
//! it. Durable for the same reason [`crate::ota_state`] is: an install **deliberately restarts the
//! edge** ([ADR-0055](../../../docs/adr/0055-edge-ota-updater.md)), and a held generation in process
//! memory would be re-taken from config on every boot.
//!
//! # Why this is not a port
//!
//! Same category as [`OtaStateAuthority`](crate::ota_state::OtaStateAuthority),
//! [`QueueNumberAuthority`](crate::queue::QueueNumberAuthority) and
//! [`ReceiptAuthority`](crate::receipt::ReceiptAuthority): durable *edge-local* bookkeeping, a trait
//! `pos-edge` defines and implements **for** [`SqliteStore`] over that store's public API, with an
//! in-memory twin for tests. Nothing swaps in a different memory of which lease a box holds, and no
//! vendor sits behind one, so it earns no `PortName` and no contract suite
//! ([ADR-0026](../../../docs/adr/0026-port-shapes.md)). [`InMemoryLease`] is held to the same
//! expectations as the SQLite path, because this decides whether a box may take an update.

use core::future::Future;

use pos_core::lease::{LeaseGeneration, LeaseStanding, lease_standing};
use pos_ports::PortError;
use pos_proto::ids::StoreId;
use store_sqlite::SqliteStore;

/// Where the store remembers the lease generation it holds, across the restart an install performs.
///
/// Two operations and no setter, deliberately. There is no way to move a held generation *forward*
/// on a running box: a machine that must legitimately hold a newer one is a machine being
/// re-provisioned, which starts from a fresh database.
pub trait LeaseAuthority: Send + Sync {
    /// Takes `generation` if the store holds none yet, and reports the one it holds either way.
    ///
    /// Idempotent, and **not** an upsert: called twice with different values, the second call
    /// returns the first value. That is the take-once rule, and it is what keeps a superseded box
    /// superseded.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the authority cannot be reached or the write fails.
    fn take(
        &self,
        store_id: StoreId,
        generation: LeaseGeneration,
    ) -> impl Future<Output = Result<LeaseGeneration, PortError>> + Send;

    /// The generation the store holds, or `None` if it has never taken one — every box, until the
    /// cloud first issues its store a lease.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the authority cannot be reached or the read fails.
    fn held(
        &self,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Option<LeaseGeneration>, PortError>> + Send;
}

impl LeaseAuthority for SqliteStore {
    async fn take(
        &self,
        store_id: StoreId,
        generation: LeaseGeneration,
    ) -> Result<LeaseGeneration, PortError> {
        self.take_lease(store_id, generation.value())
            .await
            .map(LeaseGeneration::new)
    }

    async fn held(&self, store_id: StoreId) -> Result<Option<LeaseGeneration>, PortError> {
        Ok(self.held_lease(store_id).await?.map(LeaseGeneration::new))
    }
}

/// Delegates through a shared handle, so one authority can be held by more than one loop — the OTA
/// tick weighs the standing, and the heartbeat reports the generation the box holds.
impl<T> LeaseAuthority for std::sync::Arc<T>
where
    T: LeaseAuthority,
{
    fn take(
        &self,
        store_id: StoreId,
        generation: LeaseGeneration,
    ) -> impl Future<Output = Result<LeaseGeneration, PortError>> + Send {
        (**self).take(store_id, generation)
    }

    fn held(
        &self,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Option<LeaseGeneration>, PortError>> + Send {
        (**self).held(store_id)
    }
}

/// An in-memory lease authority: one generation per store, taken once.
///
/// What the fakes-backed example and the edge tests run against. **Not durable** — a restart forgets
/// it, which is the defect the SQLite path exists to fix — but the contract it honours (take-once,
/// absent until first taken, keyed by store) is identical, so a standing proven here behaves the
/// same in production.
#[derive(Debug, Default)]
pub struct InMemoryLease {
    inner: std::sync::Mutex<std::collections::HashMap<StoreId, LeaseGeneration>>,
}

impl InMemoryLease {
    /// A fresh authority holding no lease for any store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl LeaseAuthority for InMemoryLease {
    async fn take(
        &self,
        store_id: StoreId,
        generation: LeaseGeneration,
    ) -> Result<LeaseGeneration, PortError> {
        // `or_insert` and not `insert`: the first take wins, exactly as the SQL's
        // `ON CONFLICT DO NOTHING` does.
        Ok(*self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(store_id)
            .or_insert(generation))
    }

    async fn held(&self, store_id: StoreId) -> Result<Option<LeaseGeneration>, PortError> {
        Ok(self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&store_id)
            .copied())
    }
}

/// This box's lease standing, given the authoritative generation the cloud last published.
///
/// - **`published` is `None`** — the store has never been issued a lease, so there is nothing to be
///   superseded by and the box is [`LeaseStanding::Active`]. This is every store until an operator
///   deliberately issues one, and it is what makes the mechanism safe to ship to a fleet that has
///   never had it: behaviour is unchanged until the day a store is given a lease.
/// - **`published` is `Some`** — the box takes it if it holds nothing, then compares. A box that
///   already held a generation compares *that* one, which is the entire point.
///
/// # Errors
///
/// [`PortError`] if the held generation could not be read or taken. The caller must **not** treat
/// that as `Active`: a box that cannot read its own lease has not established that it is the store,
/// and weighing it as though it had is the failure this module exists to remove.
pub async fn standing<A>(
    authority: &A,
    store_id: StoreId,
    published: Option<LeaseGeneration>,
) -> Result<LeaseStanding, PortError>
where
    A: LeaseAuthority,
{
    let Some(authoritative) = published else {
        return Ok(LeaseStanding::Active);
    };
    let held = authority.take(store_id, authoritative).await?;
    Ok(lease_standing(held, authoritative))
}

#[cfg(test)]
mod tests {
    use super::{InMemoryLease, LeaseAuthority, standing};
    use pos_core::lease::{LeaseGeneration, LeaseStanding};
    use pos_proto::ids::StoreId;
    use pos_proto::ulid::Ulid;

    fn store() -> StoreId {
        StoreId::new(Ulid::from_u128(7))
    }

    fn generation(value: u64) -> LeaseGeneration {
        LeaseGeneration::new(value)
    }

    #[tokio::test]
    async fn a_store_with_no_published_lease_is_active_exactly_as_before() {
        let authority = InMemoryLease::new();
        assert_eq!(
            standing(&authority, store(), None).await.expect("standing"),
            LeaseStanding::Active,
            "a fleet that has never been issued a lease must behave as it did"
        );
        assert!(
            authority.held(store()).await.expect("read").is_none(),
            "and nothing is taken, so the first real lease is still a first sight"
        );
    }

    #[tokio::test]
    async fn the_first_sight_is_taken_and_reads_active() {
        let authority = InMemoryLease::new();
        assert_eq!(
            standing(&authority, store(), Some(generation(4)))
                .await
                .expect("standing"),
            LeaseStanding::Active
        );
        assert_eq!(
            authority.held(store()).await.expect("read"),
            Some(generation(4))
        );
    }

    #[tokio::test]
    async fn a_replacement_supersedes_the_box_and_it_stays_superseded() {
        let authority = InMemoryLease::new();
        // This box comes up under generation 4 and is the store.
        assert_eq!(
            standing(&authority, store(), Some(generation(4)))
                .await
                .expect("standing"),
            LeaseStanding::Active
        );

        // A replacement is activated; the cloud bumps to 5 and publishes it. This box learns it on
        // its next config pull.
        assert_eq!(
            standing(&authority, store(), Some(generation(5)))
                .await
                .expect("standing"),
            LeaseStanding::Superseded
        );

        // The load-bearing assertion: it did **not** adopt 5. Re-adopting is the bug that makes the
        // whole mechanism decorative, so pull again and again — the verdict must not drift back.
        assert_eq!(
            authority.held(store()).await.expect("read"),
            Some(generation(4))
        );
        for _ in 0..3_u8 {
            assert_eq!(
                standing(&authority, store(), Some(generation(5)))
                    .await
                    .expect("standing"),
                LeaseStanding::Superseded,
                "a superseded box must not re-promote itself on the next pull"
            );
        }
    }

    #[tokio::test]
    async fn a_generation_behind_what_the_box_holds_is_invalid_for_everyone() {
        // A config rollback can move the published node backwards even though it cannot move the
        // cloud's table. Take-once makes that fail safe: every box reads `Invalid` and refuses,
        // rather than one of them being wrongly promoted.
        let authority = InMemoryLease::new();
        standing(&authority, store(), Some(generation(5)))
            .await
            .expect("standing");
        assert_eq!(
            standing(&authority, store(), Some(generation(3)))
                .await
                .expect("standing"),
            LeaseStanding::Invalid
        );
    }

    #[tokio::test]
    async fn the_standing_is_scoped_to_its_store() {
        let authority = InMemoryLease::new();
        standing(&authority, store(), Some(generation(9)))
            .await
            .expect("standing");
        let other = StoreId::new(Ulid::from_u128(8));
        assert!(authority.held(other).await.expect("read").is_none());
        assert_eq!(
            standing(&authority, other, Some(generation(2)))
                .await
                .expect("standing"),
            LeaseStanding::Active,
            "another store's first sight is its own"
        );
    }
}
