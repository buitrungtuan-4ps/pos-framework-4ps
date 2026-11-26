// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An order its guest paid for in a delivery vendor's app is settled "Paid via <vendor>" when the
//! store accepts it
//! ([ADR-0248](../../../../../docs/adr/0248-a-delivery-order-is-priced-paid-and-reconciled-as-the-app-sold-it.md)
//! decision 2).
//!
//! The box acts for itself, as it does when it sends an order on arrival (ADR-0247): its events
//! name the store's own device and no employee, and it holds only the two permissions the act
//! needs. No money reaches a drawer, so no till is involved; the bill settles at its whole total,
//! which under the app's price source is what the guest paid, and issues its e-invoice at the
//! store's issue moment as any settled bill does.

use pos_core::billing::Payment;
use pos_core::business_date::derive_business_date;
use pos_core::decision::{Actor, DecisionCtx};
use pos_core::permission::{Permission, PermissionSet};
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::SubjectStore;
use pos_proto::ids::{DeviceId, EmployeeId, OrderId};
use pos_proto::vendors::VendorPayment;
use pos_proto::{BillState, ClockSource, PaymentMethod};

use crate::app::{AppError, BillView, Edge, TillScope};

impl<S: EventStore> Edge<S> {
    /// Settles `order_id` with [`PaymentMethod::Vendor`] for its whole total, where it came through
    /// a vendor whose guest pays in the app, opening its bill first. `Ok(None)` for any other order,
    /// and for one already settled, so calling it again does nothing.
    ///
    /// # Errors
    ///
    /// Whatever opening or settling the bill refuses: no open shift at a store that sells only in
    /// one, a check the store shows before payment, or the store not written. The bill is then left
    /// for the till, as an order paid at the store is.
    pub async fn settle_paid_in_app(&self, order_id: OrderId) -> Result<Option<BillView>, AppError>
    where
        S: SubjectStore,
    {
        let Some(vendor) = self.order_vendor(order_id) else {
            return Ok(None);
        };
        if VendorPayment::read(&vendor.payment) != VendorPayment::InApp {
            return Ok(None);
        }
        let ctx = self.box_ctx()?;
        let existing = {
            let projection = self.lock_projection();
            projection
                .bill_for_order(order_id)
                .and_then(|bill_id| projection.bill(bill_id).map(|bill| (bill_id, bill.state)))
        };
        let bill_id = match existing {
            Some((_, BillState::Settled)) => return Ok(None),
            Some((bill_id, _)) => bill_id,
            None => self.open_bill_in(&ctx, order_id).await?.bill_id,
        };
        let total = self.order_totals(order_id)?.total_due;
        let paid = Payment {
            method: PaymentMethod::Vendor,
            tendered: total,
            applied_to_bill: total,
            tip: pos_proto::money::Money::zero(total.currency_code),
        };
        self.settle_in(
            ctx,
            TillScope::default(),
            bill_id,
            (vec![paid], None, None),
            None,
            None,
        )
        .await
        .map(Some)
    }

    /// The box acting for itself: the store's own device, no employee, and only what a settle of
    /// an order paid in the app needs. A correction decision HQ published is recorded under it too
    /// (ADR-0250).
    pub(super) fn box_ctx(&self) -> Result<DecisionCtx, AppError> {
        let session = self.session();
        let now = self.clock.now();
        let business_date = derive_business_date(now, &session.timezone, session.cutoff)
            .map_err(|_| AppError::Clock)?;
        let device_id = DeviceId::new(self.identity.store_id.as_ulid());
        let grants = PermissionSet::EMPTY
            .with(Permission::OpenBill)
            .with(Permission::TakePayment);
        Ok(DecisionCtx {
            now,
            business_date,
            actor: Actor {
                employee_id: EmployeeId::new(device_id.as_ulid()),
                device_id,
            },
            granted: grants,
            granted_directly: grants,
            capabilities: session.capabilities,
            connectivity: session.connectivity,
            currency: session.currency,
        })
    }
}
