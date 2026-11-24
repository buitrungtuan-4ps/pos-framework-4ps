// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a kiosk may reach: the orders it opened itself
//! ([ADR-0224](../../../../../docs/adr/0224-self-order-is-a-guest-page-and-a-kiosk.md) decision 4).
//!
//! A kiosk's own record acts on the fixed kiosk set alone (`KIOSK_GRANTS`), which already keeps it
//! from taking money, discounting or opening a drawer. These answer the second half: a guest at a
//! kiosk reads and changes only the order the kiosk opened for them, never another till's.

use pos_ports::event_store::EventStore;
use pos_proto::ids::{EmployeeId, OrderId, OrderLineId};

use crate::app::Edge;

impl<S: EventStore> Edge<S> {
    /// Whether `employee_id` is a kiosk's own staff record rather than a person.
    #[must_use]
    pub fn is_kiosk(&self, employee_id: EmployeeId) -> bool {
        self.session().staff.is_kiosk(employee_id)
    }

    /// Whether `employee_id` opened `order_id` and the order is still open.
    #[must_use]
    pub fn opened_by(&self, order_id: OrderId, employee_id: EmployeeId) -> bool {
        self.lock_projection().opener(order_id) == Some(employee_id)
    }

    /// The order `line_id` is on, if the store knows the line.
    #[must_use]
    pub fn order_of_line(&self, line_id: OrderLineId) -> Option<OrderId> {
        self.lock_projection()
            .line(line_id)
            .map(|record| record.order_id)
    }

    /// The order `bill_id` is for, if the store knows the bill.
    #[must_use]
    pub fn order_of_bill(&self, bill_id: pos_proto::ids::BillId) -> Option<OrderId> {
        self.lock_projection()
            .bill(bill_id)
            .map(|record| record.order_id)
    }
}
