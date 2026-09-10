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
use std::sync::{Arc, Mutex, PoisonError};

use pos_core::lease::{LeaseGeneration, LeaseStanding, lease_standing};
use pos_ports::PortError;
use pos_proto::ids::StoreId;
use store_sqlite::SqliteStore;

/// A hand-boxed future, so [`HeldLease`] can be a trait object. The workspace takes no
/// `async-trait`, and adding one for a single method would be a dependency decision of its own.
type BoxFuture<'a, T> = core::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Where the store remembers the lease generation it holds, across the restart an install performs.
///
/// Two operations and no setter, deliberately, plus one eraser. There is no way to move a held
/// generation *forward* on a running box: a machine that must legitimately hold a newer one is a
/// machine being re-provisioned. [`forget`](Self::forget) is what makes that sentence true when the
/// re-provisioning reuses the *data* — see its own doc.
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

    /// Forgets the held generation, so the next [`take`](Self::take) is a first sight again.
    ///
    /// # Not a setter, and called from exactly one place
    ///
    /// This is not a way to move a generation forward — it cannot name one. It is the answer to a
    /// hole take-once leaves, found while building
    /// [ADR-0123](../../../docs/adr/0123-a-superseded-box-opens-nothing-new.md): take-once was meant
    /// to bind a generation to a *box*, and it actually binds it to a **disk**. That is a
    /// distinction without a difference until somebody moves the disk — and
    /// `docs/guides/bring-a-store-online.md` tells an operator replacing a dead machine to do
    /// exactly that, because copying `store.sqlite` is the only way to recover events the old box
    /// committed and never published. Without this, the replacement boots holding the dead box's
    /// generation, reads itself `Superseded`, and ADR-0123's gate stops the *new* machine seating
    /// tables: the "fires wrongly" failure ADR-0108 spent a paragraph warning about.
    ///
    /// Activation is the only caller, because it is the only moment "this disk is now a different
    /// machine" is a fact rather than a guess: the box is being issued its own cloud credential, and
    /// a box that already holds one is refused a second activation. Everything else — every pull,
    /// every restart, every install — still only ever takes and compares.
    ///
    /// Idempotent: forgetting a generation the store does not hold is a no-op.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the authority cannot be reached or the write fails.
    fn forget(&self, store_id: StoreId) -> impl Future<Output = Result<(), PortError>> + Send;
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

    async fn forget(&self, store_id: StoreId) -> Result<(), PortError> {
        self.forget_lease(store_id).await
    }
}

/// Delegates through a shared handle, so one authority can be held by more than one loop — the OTA
/// tick weighs the standing, and the heartbeat reports the generation the box holds.
impl<T> LeaseAuthority for Arc<T>
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

    fn forget(&self, store_id: StoreId) -> impl Future<Output = Result<(), PortError>> + Send {
        (**self).forget(store_id)
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
    inner: Mutex<std::collections::HashMap<StoreId, LeaseGeneration>>,
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
            .unwrap_or_else(PoisonError::into_inner)
            .entry(store_id)
            .or_insert(generation))
    }

    async fn held(&self, store_id: StoreId) -> Result<Option<LeaseGeneration>, PortError> {
        Ok(self
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&store_id)
            .copied())
    }

    async fn forget(&self, store_id: StoreId) -> Result<(), PortError> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&store_id);
        Ok(())
    }
}

/// The lease standing this box is acting on, shared by the loop that decides it and everything that
/// acts on it ([ADR-0123](../../../docs/adr/0123-a-superseded-box-opens-nothing-new.md)).
///
/// Shaped exactly like [`Origins`](crate::origins::Origins) and [`Pairing`](crate::pairing::Pairing),
/// and for the same reasons: interior mutability behind one `Arc`, written by the config-pull loop
/// and read by the request path — here also by the application layer, which is the reader that
/// decides whether a table may be seated.
///
/// # Why `Active` until told otherwise
///
/// A box that has never pulled a config, and a store the cloud has never issued a lease to, are both
/// [`LeaseStanding::Active`] — the fleet's behaviour before any of this existed, preserved on
/// purpose. The refusal begins the day a store is given a lease and a replacement takes it, not the
/// day this shipped.
#[derive(Debug)]
pub struct CurrentStanding {
    standing: Mutex<LeaseStanding>,
}

impl Default for CurrentStanding {
    fn default() -> Self {
        Self {
            standing: Mutex::new(LeaseStanding::Active),
        }
    }
}

impl CurrentStanding {
    /// A fresh cell reading [`LeaseStanding::Active`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The standing this box is currently acting on.
    #[must_use]
    pub fn get(&self) -> LeaseStanding {
        *self.standing.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records a freshly weighed standing.
    ///
    /// Only [`LeaseWatch::settle`] should call this on a running box: a standing set from anywhere
    /// else is a standing that did not come from the box's own held generation.
    pub fn set(&self, standing: LeaseStanding) {
        *self.standing.lock().unwrap_or_else(PoisonError::into_inner) = standing;
    }

    /// Whether this box may **open** something new — a new order, or a new shift (ADR-0123).
    ///
    /// `false` for [`LeaseStanding::Superseded`] alone. [`LeaseStanding::Invalid`] keeps trading:
    /// under take-once its likeliest cause is a config rollback moving the published node backwards,
    /// which would read `Invalid` on *every* box in the store at once — turning an admin's rollback
    /// into a shop that cannot seat a table. That is a different judgement from the OTA gate beside
    /// it, which refuses an install on both, and the difference is deliberate: nothing is lost by
    /// not installing.
    #[must_use]
    pub fn opens_new_work(&self) -> bool {
        match self.get() {
            LeaseStanding::Active | LeaseStanding::Invalid => true,
            LeaseStanding::Superseded => false,
        }
    }

    /// The wire token for this standing — what `GET /api/session` reports and what the
    /// `pos-lease-standing` response header carries.
    #[must_use]
    pub fn token(&self) -> &'static str {
        match self.get() {
            LeaseStanding::Active => "active",
            LeaseStanding::Superseded => "superseded",
            LeaseStanding::Invalid => "invalid",
        }
    }
}

/// The box's own lease in the one shape the config-pull loop needs it: hand it the generation the
/// cloud last published, get back what this box is.
///
/// A trait object rather than a type parameter on [`ConfigClient`](crate::config_client::ConfigClient),
/// which is the same call [`DeviceRevocations`](crate::device_revocations::DeviceRevocations) made
/// with its `Arc<dyn DurableAuth>`: a parameter would reach every test and example that builds a
/// config loop, and none of them has a lease. It is a *narrower* trait than [`LeaseAuthority`] and
/// not a replacement for it — the loop has no business taking or reading a generation, only weighing
/// one — and it is separate because [`LeaseAuthority`] returns `impl Future` and therefore cannot be
/// a trait object at all.
pub trait HeldLease: Send + Sync {
    /// This box's standing, given the authoritative generation the cloud last published.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the held generation could not be read or taken.
    fn weigh(
        &self,
        published: Option<LeaseGeneration>,
    ) -> BoxFuture<'_, Result<LeaseStanding, PortError>>;

    /// Forgets the held generation — [`LeaseAuthority::forget`] through the object.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the write fails.
    fn forget(&self) -> BoxFuture<'_, Result<(), PortError>>;
}

/// [`HeldLease`] for one store over any [`LeaseAuthority`] — the adapter that erases the authority's
/// type so the config loop can hold it.
#[derive(Debug)]
pub struct StoreLease<A> {
    authority: A,
    store_id: StoreId,
}

impl<A> StoreLease<A> {
    /// This store's lease over `authority`.
    pub const fn new(authority: A, store_id: StoreId) -> Self {
        Self {
            authority,
            store_id,
        }
    }
}

impl<A: LeaseAuthority> HeldLease for StoreLease<A> {
    fn weigh(
        &self,
        published: Option<LeaseGeneration>,
    ) -> BoxFuture<'_, Result<LeaseStanding, PortError>> {
        Box::pin(standing(&self.authority, self.store_id, published))
    }

    fn forget(&self) -> BoxFuture<'_, Result<(), PortError>> {
        Box::pin(self.authority.forget(self.store_id))
    }
}

/// What the config-pull loop is handed so that a pull settles this box's standing: the box's own
/// lease, and the shared cell every reader answers from.
///
/// The carrier exists for the same reason `DeviceRevocations` does — the loop derives `Debug` and a
/// trait object does not — and it is where the never-blank rule for the standing lives.
pub struct LeaseWatch {
    lease: Arc<dyn HeldLease>,
    current: Arc<CurrentStanding>,
}

/// A name and nothing else: one field is a trait object, and the other is a lock whose contents are
/// already on every `/api/*` response.
impl core::fmt::Debug for LeaseWatch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("LeaseWatch")
    }
}

impl LeaseWatch {
    /// The carrier `compose` hands to the config loop and to the boot restore.
    #[must_use]
    pub fn new(lease: Arc<dyn HeldLease>, current: Arc<CurrentStanding>) -> Self {
        Self { lease, current }
    }

    /// Forgets a lease generation this box inherited, and reads `Active` again
    /// ([ADR-0123](../../../docs/adr/0123-a-superseded-box-opens-nothing-new.md)).
    ///
    /// Activation calls this, and nothing else does. The two halves go together on purpose: the
    /// durable row is what would otherwise re-supersede the box on its next pull, and the shared
    /// cell is what the till is refusing on *right now* — forgetting one without the other would
    /// leave a freshly provisioned machine refusing to seat a table until a config pull landed.
    ///
    /// A failure is logged and the standing is left alone, for the same reason
    /// [`Self::settle`]'s is: the box has just been given its own credential and is about to be
    /// asked to trade, and a write that did not land is not a reason to decide it is somebody
    /// else's machine. The next activation cannot retry it — there is no next activation — so this
    /// logs at `error`, unlike `settle`'s `warn`.
    pub async fn adopt_this_box(&self) {
        match self.lease.forget().await {
            Ok(()) => {
                self.current.set(LeaseStanding::Active);
                tracing::info!(
                    "activation cleared any lease generation this box inherited; the next config \
                     pull takes the store's current one"
                );
            }
            Err(error) => tracing::error!(
                %error,
                "activation could not clear an inherited lease generation; if this box was built \
                 from a copy of another box's store.sqlite it may refuse to open new orders until \
                 it is provisioned from an empty database"
            ),
        }
    }

    /// Weighs the generation a pull just published and records the answer.
    ///
    /// # A read that fails changes nothing, in either direction
    ///
    /// The previous standing stands — the never-blank rule the same loop applies to every config
    /// node, and here it cuts both ways on purpose. A SQLite hiccup must not stop a shop seating
    /// tables, and it must not un-supersede a box a replacement has already taken either.
    /// [`standing`] already refuses to answer `Active` on an unreadable lease; this is the other
    /// half of that sentence, on the side where the failure costs money.
    pub async fn settle(&self, published: Option<LeaseGeneration>) {
        match self.lease.weigh(published).await {
            Ok(weighed) => {
                let previous = self.current.get();
                self.current.set(weighed);
                if previous != weighed {
                    tracing::info!(
                        ?previous,
                        standing = ?weighed,
                        "this box's lease standing changed"
                    );
                }
            }
            Err(error) => tracing::warn!(
                %error,
                "could not weigh this box's lease standing; keeping the standing it was already \
                 acting on, because reading it as active would let a replaced machine open orders"
            ),
        }
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
