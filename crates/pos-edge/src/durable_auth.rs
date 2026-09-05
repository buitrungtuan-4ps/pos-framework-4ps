// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge's object-safe view of [`DeviceRegistry`]
//! ([ADR-0091](../../../docs/adr/0091-durable-edge-auth-state.md)).
//!
//! # Why this is here and not in `pos-ports`
//!
//! `pos-ports` already has [`pos_ports::dynamic`], and this deliberately does **not** go there.
//! That module reserves its mirrors for *"the four families that need runtime selection"* — several
//! printers, two acquirers, two marketplaces — and says in as many words that the other ports have
//! none because they are chosen once when a binary starts. `DeviceRegistry` is the second kind:
//! which adapter implements it is decided by what `main.rs` composes, not by configuration.
//!
//! The reason a trait object is needed at all is a **`pos-edge`** constraint, so the abstraction
//! belongs where the constraint lives: [`AppState`](crate::AppState) is not generic, and making it
//! generic would ripple through [`crate::http::router`], both auth middlewares, and every test that
//! builds a state. So the boxing happens here, paid once per pairing or sign-in rather than on a
//! request path.
//!
//! # How the blanket implementation avoids a new parameter
//!
//! `Edge<S>` exposes its store, and `serve` already holds an `Arc<Edge<S>>`. So the blanket impl
//! below is on `Edge<S>` — forwarding to `self.store()` — which means the *existing* `Arc` becomes
//! the registry with no new argument to `serve`, no change in `main.rs`, and no `S: Clone` bound.
//! An adapter implements only [`DeviceRegistry`]; this cannot drift from it, because a method that
//! stopped matching would fail to compile the blanket impl rather than silently diverge.

use core::future::Future;
use core::pin::Pin;

use pos_ports::device_registry::{DeviceRegistry, DeviceSession, PairedDevice};
use pos_ports::error::PortError;
use pos_proto::ids::DeviceId;
use pos_proto::time::Timestamp;

use crate::app::Edge;

/// A boxed future, as a trait object must return.
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// [`DeviceRegistry`] behind a trait object, so the non-generic [`AppState`](crate::AppState) can
/// hold one.
///
/// Only the methods the edge actually calls are mirrored. `paired_devices` and `sign_ins` are here
/// because boot reads them; the rest are the write paths.
///
/// [`DeviceRegistry::device_for_token`] is deliberately **not** mirrored, and was removed once it
/// was found to have no caller (production-readiness **X2**). The edge never resolves one digest
/// against storage: [`Pairing::load`](crate::pairing::Pairing::load) reads the whole table once at
/// boot and the request gate then answers from memory, which is the point of holding the map at
/// all. A mirror nothing calls is worse than no mirror — it is surface an adapter author reads as a
/// requirement, and its old doc here claimed a "boot path consistency check" that was never
/// written. The port keeps the method: it is a real capability of a registry and the contract suite
/// asserts the digest binding through it.
pub trait DurableAuth: Send + Sync {
    /// See [`DeviceRegistry::record_pairing`].
    fn record_pairing(&self, device: PairedDevice) -> BoxFuture<'_, Result<(), PortError>>;
    /// See [`DeviceRegistry::paired_devices`].
    fn paired_devices(&self) -> BoxFuture<'_, Result<Vec<PairedDevice>, PortError>>;
    /// See [`DeviceRegistry::revoke_device`].
    fn revoke_device(&self, device_id: DeviceId) -> BoxFuture<'_, Result<(), PortError>>;
    /// See [`DeviceRegistry::revoke_all_devices`].
    fn revoke_all_devices(&self) -> BoxFuture<'_, Result<(), PortError>>;
    /// See [`DeviceRegistry::record_sign_in`].
    fn record_sign_in(&self, session: DeviceSession) -> BoxFuture<'_, Result<(), PortError>>;
    /// See [`DeviceRegistry::sign_ins`].
    fn sign_ins(&self) -> BoxFuture<'_, Result<Vec<DeviceSession>, PortError>>;
    /// See [`DeviceRegistry::touch_session`].
    fn touch_session(
        &self,
        device_id: DeviceId,
        now: Timestamp,
    ) -> BoxFuture<'_, Result<(), PortError>>;
    /// See [`DeviceRegistry::clear_sign_in`].
    fn clear_sign_in(&self, device_id: DeviceId) -> BoxFuture<'_, Result<(), PortError>>;
}

/// Every `DeviceRegistry` is one, so an adapter implements only the plain trait.
impl<T: DeviceRegistry> DurableAuth for T {
    fn record_pairing(&self, device: PairedDevice) -> BoxFuture<'_, Result<(), PortError>> {
        Box::pin(DeviceRegistry::record_pairing(self, device))
    }

    fn paired_devices(&self) -> BoxFuture<'_, Result<Vec<PairedDevice>, PortError>> {
        Box::pin(DeviceRegistry::paired_devices(self))
    }

    fn revoke_device(&self, device_id: DeviceId) -> BoxFuture<'_, Result<(), PortError>> {
        Box::pin(DeviceRegistry::revoke_device(self, device_id))
    }

    fn revoke_all_devices(&self) -> BoxFuture<'_, Result<(), PortError>> {
        Box::pin(DeviceRegistry::revoke_all_devices(self))
    }

    fn record_sign_in(&self, session: DeviceSession) -> BoxFuture<'_, Result<(), PortError>> {
        Box::pin(DeviceRegistry::record_sign_in(self, session))
    }

    fn sign_ins(&self) -> BoxFuture<'_, Result<Vec<DeviceSession>, PortError>> {
        Box::pin(DeviceRegistry::sign_ins(self))
    }

    fn touch_session(
        &self,
        device_id: DeviceId,
        now: Timestamp,
    ) -> BoxFuture<'_, Result<(), PortError>> {
        Box::pin(DeviceRegistry::touch_session(self, device_id, now))
    }

    fn clear_sign_in(&self, device_id: DeviceId) -> BoxFuture<'_, Result<(), PortError>> {
        Box::pin(DeviceRegistry::clear_sign_in(self, device_id))
    }
}

/// The composed edge is the registry, by forwarding to the store it already owns.
///
/// This is what lets `serve` reuse its existing `Arc<Edge<S>>` instead of taking a new parameter.
///
/// A newtype rather than `impl DurableAuth for Edge<S>` directly, so it cannot collide with the
/// blanket impl above: `Edge<S>` is not a `DeviceRegistry` today, but if it ever became one the two
/// impls would overlap and the error would surface far from here. Wrapping keeps the two cases
/// disjoint by construction.
#[expect(
    missing_debug_implementations,
    reason = "a wrapper around Arc<Edge<S>>, which is not Debug either; nothing logs it"
)]
pub struct EdgeRegistry<S>(pub std::sync::Arc<Edge<S>>);

impl<S: pos_ports::EventStore + DeviceRegistry + Send + Sync> DurableAuth for EdgeRegistry<S> {
    fn record_pairing(&self, device: PairedDevice) -> BoxFuture<'_, Result<(), PortError>> {
        Box::pin(DeviceRegistry::record_pairing(self.0.store(), device))
    }

    fn paired_devices(&self) -> BoxFuture<'_, Result<Vec<PairedDevice>, PortError>> {
        Box::pin(DeviceRegistry::paired_devices(self.0.store()))
    }

    fn revoke_device(&self, device_id: DeviceId) -> BoxFuture<'_, Result<(), PortError>> {
        Box::pin(DeviceRegistry::revoke_device(self.0.store(), device_id))
    }

    fn revoke_all_devices(&self) -> BoxFuture<'_, Result<(), PortError>> {
        Box::pin(DeviceRegistry::revoke_all_devices(self.0.store()))
    }

    fn record_sign_in(&self, session: DeviceSession) -> BoxFuture<'_, Result<(), PortError>> {
        Box::pin(DeviceRegistry::record_sign_in(self.0.store(), session))
    }

    fn sign_ins(&self) -> BoxFuture<'_, Result<Vec<DeviceSession>, PortError>> {
        Box::pin(DeviceRegistry::sign_ins(self.0.store()))
    }

    fn touch_session(
        &self,
        device_id: DeviceId,
        now: Timestamp,
    ) -> BoxFuture<'_, Result<(), PortError>> {
        Box::pin(DeviceRegistry::touch_session(
            self.0.store(),
            device_id,
            now,
        ))
    }

    fn clear_sign_in(&self, device_id: DeviceId) -> BoxFuture<'_, Result<(), PortError>> {
        Box::pin(DeviceRegistry::clear_sign_in(self.0.store(), device_id))
    }
}

/// Keeps the compiler honest about what this module is for: a method taking `impl Trait`, or
/// returning a bare `impl Future`, would break object safety without touching a call site.
const _: () = {
    const fn assert_dyn_compatible<T: ?Sized>() {}
    let _ = assert_dyn_compatible::<dyn DurableAuth>;
};
