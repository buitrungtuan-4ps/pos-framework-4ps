// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A counter that takes the money first
//! ([ADR-0222](../../../../../docs/adr/0222-items-carry-barcodes-and-plus-and-a-counter-can-pay-first.md) decision 6):
//! on a store with `pay_first_enabled`, settling a tableless order sends its unsent lines to the
//! kitchen in the settle's own transaction, so the kitchen sees the order once it is paid
//! (ADR-0161 keeps a paid tableless order on the board until it is done).

use pos_core::capability::Capability;
use pos_core::decision::DecisionCtx;
use pos_ports::event_store::EventStore;
use pos_proto::ids::BillId;

use crate::app::kitchen::PreparedFires;
use crate::app::{AppError, Edge, StaffStanding};

impl<S: EventStore> Edge<S> {
    /// The fires a settle of `bill_id` carries: on a pay-first store, for a bill with no table,
    /// every unsent line it covers that the published plan routes to a station. A line no station
    /// takes stays unsent, as it would on a store with no kitchen screen, and the settle goes ahead;
    /// a guest's order staff have not confirmed fires nothing. Nothing anywhere else.
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] for a line that may not fire.
    pub(in crate::app) fn fires_at_settle(
        &self,
        ctx: &DecisionCtx,
        bill_id: BillId,
        tableless: bool,
    ) -> Result<PreparedFires, AppError> {
        let session = self.session();
        let none = || Ok((Vec::new(), Vec::new(), Vec::new()));
        if !tableless || !session.capabilities.enabled(Capability::PayFirst) {
            return none();
        }
        let unfired = {
            let projection = self.lock_projection();
            let Some(bill) = projection.bill(bill_id) else {
                return none();
            };
            if projection.staff_standing(bill.order_id, &session) != StaffStanding::Fireable {
                return none();
            }
            let covered = projection.covered_lines(&bill);
            projection
                .unfired_lines_for_order(bill.order_id)
                .into_iter()
                .filter(|(id, record)| {
                    covered.contains(id)
                        && session
                            .resolve_station(record.menu_item_id, record.course_id)
                            .is_some()
                })
                .collect::<Vec<_>>()
        };
        self.prepare_fires(ctx, unfired, |_| None, None)
    }
}
