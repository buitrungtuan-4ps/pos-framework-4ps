// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Which drawer an act is on, and the refusals that keep a sale or a movement to an open one.

use pos_core::billing::Payment;
use pos_core::decision::DecisionCtx;
use pos_ports::event_store::EventStore;
use pos_proto::PaymentMethod;
use pos_proto::ids::{DeviceId, ShiftId};
use pos_proto::shift::{DrawerModel, NoShiftSelling};

use crate::app::records::is_till;
use crate::app::{AppError, Approval, Edge, SettleDrawer, TillScope};

impl<S: EventStore> Edge<S> {
    /// The shift of the drawer the device `scope` says acts at, trading or counted: the store's
    /// one drawer's, or where the store keeps one per till the device's own till's, which a device
    /// that is no till has none of. A payment taken there is stamped with it (ADR-0167 decision 5).
    pub(in crate::app) fn drawer_shift(&self, scope: TillScope) -> Option<ShiftId> {
        let drawer = match self.drawer_model() {
            DrawerModel::PerTerminal => Some(scope.own?),
            _ => None,
        };
        self.lock_projection().open_shift_of(drawer)
    }

    /// The drawer cash taken or moved at the device `scope` says acts goes into: the store's one
    /// drawer, or where the store keeps one per till the device's own till's
    /// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6).
    ///
    /// # Errors
    ///
    /// [`AppError::NotATill`] where the store keeps a drawer per till and the device is none of its
    /// tills, which takes any other tender but no cash.
    pub(in crate::app) fn cash_drawer(
        &self,
        scope: TillScope,
    ) -> Result<Option<DeviceId>, AppError> {
        if self.drawer_model() != DrawerModel::PerTerminal {
            return Ok(None);
        }
        match scope.own {
            Some(till) if self.is_published_till(till) => Ok(Some(till)),
            _ => Err(AppError::NotATill),
        }
    }

    /// Whether `till` is a `TERMINAL` entry on the store's `devices` node.
    pub(in crate::app) fn is_published_till(&self, till: DeviceId) -> bool {
        self.session()
            .devices
            .devices()
            .iter()
            .any(|device| device.device_id == till && is_till(device))
    }

    /// The store's one drawer's shift, trading or counted, so every event minted during it carries
    /// its id. `None` when it is not open, which is the ordinary state before the first open of the
    /// day and every state where the store keeps a drawer per till, whose events name their shift
    /// where a drawer's cash is concerned (ADR-0167 decision 5).
    pub(in crate::app) fn current_shift_id(&self) -> Option<ShiftId> {
        self.lock_projection().open_shift_of(None)
    }

    /// Refuses a sale while no shift is open, when the store's `shift` node says
    /// `NO_SHIFT_SELLING_REFUSE`
    /// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)).
    ///
    /// Called where a sale starts, seating a table or starting a counter order, and where a payment
    /// is taken, so that at such a store every sale and every payment belongs to a shift. An order a
    /// guest or a delivery partner sends is still taken, because nobody is at the till to open a
    /// shift for it; its payment waits for one. Under the default, `NO_SHIFT_SELLING_ALLOW`, the
    /// store sells as it always has.
    ///
    /// Where the store keeps a drawer per till, any till's open shift lets a sale start and a tender
    /// other than cash be taken; cash asks [`Self::refuse_without_open_drawer`] instead.
    pub(in crate::app) fn refuse_without_open_shift(&self) -> Result<(), AppError> {
        if self.session().shift.no_shift_selling() == NoShiftSelling::Refuse
            && self.lock_projection().open_shifts.is_empty()
        {
            return Err(AppError::OpenShiftRequired);
        }
        Ok(())
    }

    /// The drawer a settle's cash goes into, the till's where the store keeps one per till
    /// (ADR-0167 decision 6), and `None` for the store's one drawer or a settle that takes none;
    /// the shift its payments are stamped with, [`Self::drawer_shift`]'s; and the approver of the
    /// override cash into a drawer assigned to somebody else takes (decision 9), where one approved.
    ///
    /// # Errors
    ///
    /// [`AppError::NotATill`] for cash at a device that is no till where the store keeps a drawer
    /// per till, [`AppError::OpenShiftRequired`] where the store refuses selling without a shift
    /// and that drawer, or for any other tender every drawer, has none open, and
    /// [`Self::override_assignment`]'s refusals for cash into somebody else's drawer.
    pub(in crate::app) fn settle_drawer(
        &self,
        ctx: &mut DecisionCtx,
        scope: TillScope,
        payments: &[Payment],
        approval: Option<&Approval>,
    ) -> Result<SettleDrawer, AppError> {
        let mut overrider = None;
        let drawer = if payments
            .iter()
            .any(|payment| payment.method == PaymentMethod::Cash)
        {
            let drawer = self.cash_drawer(scope)?;
            self.refuse_without_open_drawer(drawer)?;
            let open = {
                let projection = self.lock_projection();
                projection
                    .open_shift_of(drawer)
                    .and_then(|shift_id| Some((shift_id, projection.shift(shift_id)?)))
            };
            if let Some((shift_id, record)) = open {
                self.refuse_after_day_end(shift_id, &record)?;
                overrider = self.override_assignment(ctx, Some(&record), approval)?;
            }
            drawer
        } else {
            self.refuse_without_open_shift()?;
            None
        };
        Ok((drawer, self.drawer_shift(scope), overrider))
    }

    /// [`Self::refuse_without_open_shift`] for cash, which goes into one drawer, `drawer`, whose own
    /// shift must be open: the store's one drawer's, or a till's where the store keeps one per till
    /// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6).
    fn refuse_without_open_drawer(&self, drawer: Option<DeviceId>) -> Result<(), AppError> {
        if self.session().shift.no_shift_selling() == NoShiftSelling::Refuse
            && self.lock_projection().open_shift_of(drawer).is_none()
        {
            return Err(AppError::OpenShiftRequired);
        }
        Ok(())
    }
}
