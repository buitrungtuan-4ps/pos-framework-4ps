// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Couriers.
//!
//! Ahamove and Grab Express today. `docs/architecture.md` §6.1 gives this port its three
//! operations by name — create, cancel, track — and adds the rule that matters: **courier
//! status becomes an event**. A delivery that changed state and left no event is a delivery
//! nobody can reconstruct, and this is the one port whose most important traffic arrives
//! unsolicited.
//!
//! # Callbacks arrive as events, and this port does not receive them
//!
//! A courier's webhook lands on `pos_cloud`'s HTTP surface, is verified there, and becomes a
//! domain event. It does not come back through this trait, because a port method that waits
//! for someone to call in is not a port — it is a server. [`ShipmentUpdate`] is the shape that
//! crosses either way, so the polling path in [`ShippingDispatch::track`] and the callback path
//! produce the same value and the same event.
//!
//! # A delivery address is personal data
//!
//! `docs/roadmap.md` A6 lists the Grab order name, phone and address as personal data flowing
//! through a system with no CRM. So [`DeliveryContact`] holds a `subject_id` and the details
//! stay in the side table, exactly as an invoice buyer does.

use core::fmt;
use core::future::Future;

use pos_proto::money::Money;
use pos_proto::wire_enum::Open;
use pos_proto::{OrderId, ShipmentId, ShipmentStatus, StoreId, SubjectId, Timestamp};

use crate::error::PortError;

/// The courier's identifier for a job.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CourierJobRef(Box<str>);

impl CourierJobRef {
    /// Wraps a courier reference.
    #[must_use]
    pub fn new(reference: impl Into<Box<str>>) -> Self {
        Self(reference.into())
    }

    /// The reference as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CourierJobRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for CourierJobRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CourierJobRef({})", self.0)
    }
}

/// Where a delivery is going, and to whom.
///
/// Personal data. Held here for the duration of the delivery, keyed by
/// [`Self::subject_id`] so the event log can refer to the delivery without holding the
/// recipient.
#[derive(Clone, PartialEq, Eq)]
pub struct DeliveryContact {
    /// What the event log carries instead of the details below.
    pub subject_id: SubjectId,
    /// Who is receiving it.
    pub recipient_name: String,
    /// How the courier reaches them.
    pub recipient_phone: String,
    /// Where it is going.
    pub delivery_address: String,
    /// Anything the courier needs to find the door.
    pub delivery_note: Option<String>,
}

/// Deliberately redacting, unlike an invoice buyer's.
///
/// The difference is deliberate: an invoice buyer is compared in tests against a fixture, so
/// its `Debug` is derived and a separate redacting view exists for logs. A delivery contact is
/// never compared that way, so the safe rendering is the only one, and there is no `{:?}` that
/// leaks it.
impl fmt::Debug for DeliveryContact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeliveryContact")
            .field("subject_id", &self.subject_id)
            .finish_non_exhaustive()
    }
}

/// A request to have something collected and delivered.
#[derive(Clone, Debug, PartialEq)]
pub struct DeliveryRequest {
    /// The framework's identifier for this shipment. The idempotency key.
    pub shipment_id: ShipmentId,
    /// Which store is sending it.
    pub store_id: StoreId,
    /// Which order it belongs to.
    pub order_id: OrderId,
    /// Where and to whom.
    pub contact: DeliveryContact,
    /// What the store will pay, when the courier quotes up front.
    pub quoted_fee: Option<Money>,
    /// When the food will be ready for collection.
    pub ready_at: Timestamp,
}

/// A courier job as the courier currently sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Shipment {
    /// Our identifier.
    pub shipment_id: ShipmentId,
    /// Theirs.
    pub courier_job_ref: CourierJobRef,
    /// Where it has got to. `Open`, so a courier adding a status this build predates does not
    /// deserialise as `Cancelled`.
    pub status: Open<ShipmentStatus>,
    /// What it will actually cost, once known.
    pub fee: Option<Money>,
    /// When the courier last said anything.
    pub updated_at: Timestamp,
}

/// One status change, from a poll or from a callback.
///
/// The same type either way, so both paths produce the same domain event and there is no
/// second code path that could disagree.
#[derive(Clone, Debug, PartialEq)]
pub struct ShipmentUpdate {
    /// Our identifier.
    pub shipment_id: ShipmentId,
    /// Theirs.
    pub courier_job_ref: CourierJobRef,
    /// The new status.
    pub status: Open<ShipmentStatus>,
    /// When the courier says it changed.
    pub at: Timestamp,
}

impl ShipmentUpdate {
    /// Whether this status ends the job.
    ///
    /// `false` for a status this build does not recognise, which is the safe direction: a job
    /// wrongly believed finished stops being tracked, and a job wrongly believed live is
    /// merely polled once more.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.known(),
            ShipmentStatus::Completed | ShipmentStatus::Cancelled
        )
    }
}

/// Dispatches couriers.
///
/// # Contract
///
/// 1. **`create_delivery` is idempotent by [`DeliveryRequest::shipment_id`].** Retrying does
///    not summon a second motorbike.
/// 2. **`cancel` after completion fails** with [`PortError::failed_precondition`] rather than
///    succeeding quietly. A courier who has already delivered cannot un-deliver, and a
///    successful-looking cancel would leave the store expecting a refund it will not get.
/// 3. **`cancel` of an already-cancelled job succeeds**, because cancellation is retried.
/// 4. **`track` is safe to poll**, and returns the courier's current view even for a finished
///    job, so reconciliation after a missed callback works.
pub trait ShippingDispatch: Send + Sync {
    /// Which courier this is, for logs and per-adapter metrics.
    #[must_use]
    fn courier_name(&self) -> &'static str;

    /// Books a courier.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the courier cannot be reached,
    /// [`PortError::resource_exhausted`] if no rider is available — which is a business
    /// outcome the caller surfaces rather than a fault to retry blindly — or
    /// [`PortError::invalid_argument`] if the address cannot be resolved.
    fn create_delivery(
        &self,
        request: &DeliveryRequest,
    ) -> impl Future<Output = Result<Shipment, PortError>> + Send;

    /// Cancels a booked job.
    ///
    /// # Errors
    ///
    /// [`PortError::failed_precondition`] if the job has already completed,
    /// [`PortError::not_found`] if the reference is unknown, or
    /// [`PortError::unavailable`] if the courier cannot be reached.
    fn cancel(
        &self,
        courier_job_ref: &CourierJobRef,
    ) -> impl Future<Output = Result<Shipment, PortError>> + Send;

    /// Asks where a job has got to.
    ///
    /// # Errors
    ///
    /// [`PortError::not_found`] if the reference is unknown, or
    /// [`PortError::unavailable`] if the courier cannot be reached.
    fn track(
        &self,
        courier_job_ref: &CourierJobRef,
    ) -> impl Future<Output = Result<Shipment, PortError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{CourierJobRef, DeliveryContact, ShipmentUpdate};
    use pos_proto::wire_enum::Open;
    use pos_proto::{ShipmentId, ShipmentStatus, SubjectId, Timestamp, Ulid};

    fn update(status: Open<ShipmentStatus>) -> ShipmentUpdate {
        ShipmentUpdate {
            shipment_id: ShipmentId::new(Ulid::from_u128(1)),
            courier_job_ref: CourierJobRef::new("AHA-9"),
            status,
            at: Timestamp::from_milliseconds_since_epoch(0).expect("builds"),
        }
    }

    #[test]
    fn only_completed_and_cancelled_end_a_job() {
        assert!(update(Open::from_known(ShipmentStatus::Completed)).is_terminal());
        assert!(update(Open::from_known(ShipmentStatus::Cancelled)).is_terminal());
        assert!(!update(Open::from_known(ShipmentStatus::Accepted)).is_terminal());
        assert!(!update(Open::from_known(ShipmentStatus::InTransit)).is_terminal());
    }

    #[test]
    fn an_unrecognised_status_keeps_the_job_live() {
        // The safe direction. Wrongly believing a job finished stops it being tracked;
        // wrongly believing it live costs one more poll.
        let future = update(Open::parse("SHIPMENT_STATUS_RETURNED_TO_SENDER"));
        assert!(future.status.is_unrecognised());
        assert!(!future.is_terminal());
    }

    #[test]
    fn a_delivery_contact_has_no_debug_that_leaks_it() {
        // Unlike an invoice buyer, this type has only the safe rendering — there is no
        // derived Debug to reach for by accident.
        let contact = DeliveryContact {
            subject_id: SubjectId::new(Ulid::from_u128(7)),
            recipient_name: "Tran Thi B".to_owned(),
            recipient_phone: "0901234567".to_owned(),
            delivery_address: "12 Le Loi".to_owned(),
            delivery_note: Some("ring the bell twice".to_owned()),
        };
        let logged = format!("{contact:?}");
        assert!(logged.contains("subject_id"));
        for personal in ["Tran Thi B", "0901234567", "Le Loi", "bell"] {
            assert!(
                !logged.contains(personal),
                "{personal} reached a log: {logged}"
            );
        }
    }
}
