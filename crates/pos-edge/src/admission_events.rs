// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The pairing surface's object-safe way to tell the cloud what the store admitted
//! ([ADR-0118](../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md) §4).
//!
//! # Why the seam exists
//!
//! A tablet paired during a WAN outage used to be invisible to the cloud **forever**: the heartbeat
//! carries liveness and outbox depth, the `/sync` report carries the installed version, and the
//! event catalogue's only device event was activation. So the fleet console's picture of a store's
//! tills was zero rows, a lost tablet meant sending somebody to the store, and there was nothing for
//! a remote revocation to name. `device.admission.granted` and `device.admission.revoked` ride the durable outbox — the
//! same at-least-once, idempotent-by-event-id rail every sale takes — so an admission made with the
//! cable unplugged reaches the cloud when the link returns. **Nothing here opens a channel from the
//! cloud to the store**, which is the constraint ADR-0039, ADR-0062 and ADR-0117 each refused to
//! relax.
//!
//! # Why it is a trait object, and why here rather than in `pos-ports`
//!
//! For the same reason [`DurableAuth`](crate::durable_auth::DurableAuth) is one, and the reasoning
//! there applies unchanged: [`Pairing`](crate::pairing::Pairing) is held by the non-generic
//! [`AppState`](crate::AppState), while the thing that can write an event is
//! [`Edge<S>`](crate::app::Edge), which is generic over the store. Making `AppState` generic would
//! ripple through the router, both auth middlewares and every test that builds a state. So the
//! boxing happens here, paid once per admission or revocation — a handful per store per year —
//! rather than on any request path.
//!
//! This is not a port. There is no adapter to write and no contract suite to pass: it is one edge
//! type reaching another through an interface narrow enough that a test can stand in for it. That is
//! also why there is no blanket implementation and therefore no newtype wrapper — `durable_auth`
//! needs [`EdgeRegistry`](crate::durable_auth::EdgeRegistry) only because a blanket impl over every
//! `DeviceRegistry` would overlap with one on `Edge<S>`. Nothing overlaps here, so `Edge<S>`
//! implements this directly and `serve` hands its existing `Arc<Edge<S>>` over.
//!
//! # Failure is reported and swallowed, by design
//!
//! Every method returns a result and **every caller logs it and carries on**. A device the store has
//! already admitted must not become un-admitted because an event could not be encoded, and a
//! revocation an operator performed on a lost tablet must not be undone because the outbox was full.
//! [`Edge::record_activation`](crate::app::Edge::record_activation) has always been treated this
//! way; these follow it.

use core::future::Future;
use core::pin::Pin;

use pos_ports::event_store::EventStore;
use pos_proto::ids::DeviceId;

use crate::app::{AppError, Edge};

/// A boxed future, as a trait object must return.
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Where the pairing surface reports admissions and revocations.
///
/// **No method takes an employee.** That is the interface enforcing ADR-0118 §5 rather than a
/// comment asking for it: an employee identity on these events would be a durable, central,
/// cross-border, attributable record of managerial activity, and the purpose they serve — the cloud
/// knows which tills a store admitted, and can revoke one — is served in full by device ids. Who
/// admitted a device is recorded on the store's own disk, as
/// [`PairedDevice::admitted_by`](pos_ports::device_registry::PairedDevice::admitted_by), and reaches
/// no cloud. Adding an actor here is a v2 event with its own record, and this trait is deliberately
/// shaped so that it cannot be done by accident.
pub trait AdmissionEvents: Send + Sync {
    /// The store admitted `admitted` — `admitted_by` names the paired device whose signed-in manager
    /// minted the code, and is `None` for the code a boot announces.
    fn device_admitted(
        &self,
        admitted: DeviceId,
        admitted_by: Option<DeviceId>,
    ) -> BoxFuture<'_, Result<(), AppError>>;

    /// The store retired `revoked`. One call per device, including for the break-glass that retires
    /// every device at once.
    fn device_revoked(&self, revoked: DeviceId) -> BoxFuture<'_, Result<(), AppError>>;
}

/// The composed edge is the reporter, because it already owns the store an event is appended to.
impl<S: EventStore + Send + Sync> AdmissionEvents for Edge<S> {
    fn device_admitted(
        &self,
        admitted: DeviceId,
        admitted_by: Option<DeviceId>,
    ) -> BoxFuture<'_, Result<(), AppError>> {
        Box::pin(self.record_device_admitted(admitted, admitted_by))
    }

    fn device_revoked(&self, revoked: DeviceId) -> BoxFuture<'_, Result<(), AppError>> {
        Box::pin(self.record_device_revoked(revoked))
    }
}

/// Keeps the compiler honest about what this module is for: a method taking `impl Trait`, or
/// returning a bare `impl Future`, would break object safety without touching a call site.
const _: () = {
    const fn assert_dyn_compatible<T: ?Sized>() {}
    let _ = assert_dyn_compatible::<dyn AdmissionEvents>;
};
