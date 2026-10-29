// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the edge answers about cash shifts and drawers, and the cash movements a drawer takes.

use pos_core::billing::TaxComponentLine;
use pos_core::error::DomainError;
use pos_proto::ShiftState;
use pos_proto::ids::{DeviceId, EmployeeId, ShiftId};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::reason_codes::ReasonAction;
use pos_proto::shift::{DrawerAccess, DrawerModel};
use pos_proto::text::DisplayName;
use pos_proto::time::Timestamp;

/// A closed shift's drawer, as the shift report prints it.
///
/// Every figure is one the close already computed and recorded in `cash.shift.closed`, plus the ones
/// it was computed from, so the paper adds up: the float, plus the cash taken, plus what was paid in,
/// minus what was paid out, is what was expected, and the count minus the expectation is the
/// variance ([ADR-0165](../../../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md)
/// decision 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShiftReport {
    /// The shift.
    pub shift_id: ShiftId,
    /// The float the drawer opened with.
    pub opening_float: Money,
    /// Every cash payment taken during the shift.
    pub cash_collected: Money,
    /// Cash put into the drawer outside a sale (`cash.drawer.paid_in`).
    pub paid_in: Money,
    /// Cash taken out of it outside a sale (`cash.drawer.paid_out`).
    pub paid_out: Money,
    /// The float, plus the cash taken, plus paid in, minus paid out.
    pub expected_amount: Money,
    /// What the cashier counted.
    pub counted_amount: Money,
    /// Counted minus expected. Negative means short.
    pub variance: Money,
    /// Its bills' tax by component and rate, in order of name and then rate: what the tax lines of
    /// each bill settled on the shift recorded (ADR-0168 decision 5). Empty where none recorded any,
    /// as in every store whose `tax` node names no components; a line that recorded none adds
    /// nothing, so this lists what was recorded and claims no more.
    pub tax_components: Vec<TaxComponentLine>,
}

impl ShiftReport {
    /// The drawer's figures, as its report prints them.
    #[must_use]
    pub const fn figures(&self) -> DrawerFigures {
        DrawerFigures {
            opening_float: self.opening_float,
            cash_collected: self.cash_collected,
            paid_in: self.paid_in,
            paid_out: self.paid_out,
            expected_amount: self.expected_amount,
            counted_amount: self.counted_amount,
            variance: self.variance,
        }
    }
}

/// A drawer's figures on paper, or what several drawers' came to together: the float, the cash
/// taken, paid in and out, the expectation, the count and the difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrawerFigures {
    /// The float the drawer opened with.
    pub opening_float: Money,
    /// The cash taken into it.
    pub cash_collected: Money,
    /// Cash put into it outside a sale.
    pub paid_in: Money,
    /// Cash taken out of it outside a sale.
    pub paid_out: Money,
    /// What it should hold.
    pub expected_amount: Money,
    /// What was counted.
    pub counted_amount: Money,
    /// Counted minus expected. Negative means short.
    pub variance: Money,
}

impl DrawerFigures {
    /// Each figure of `reports` summed, in `currency`: what the drawers came to together, and no
    /// figure of its own (ADR-0167 decision 10).
    pub(in crate::app) fn total(
        reports: &[ShiftReport],
        currency: CurrencyCode,
    ) -> Result<Self, DomainError> {
        let zero = Money::zero(currency);
        let none = Self {
            opening_float: zero,
            cash_collected: zero,
            paid_in: zero,
            paid_out: zero,
            expected_amount: zero,
            counted_amount: zero,
            variance: zero,
        };
        reports.iter().try_fold(none, |sum, report| {
            Ok(Self {
                opening_float: sum.opening_float.checked_add(report.opening_float)?,
                cash_collected: sum.cash_collected.checked_add(report.cash_collected)?,
                paid_in: sum.paid_in.checked_add(report.paid_in)?,
                paid_out: sum.paid_out.checked_add(report.paid_out)?,
                expected_amount: sum.expected_amount.checked_add(report.expected_amount)?,
                counted_amount: sum.counted_amount.checked_add(report.counted_amount)?,
                variance: sum.variance.checked_add(report.variance)?,
            })
        })
    }
}

/// One slip for several drawers closed at once, where the store's `shift.close_report` is
/// `CLOSE_REPORT_COMBINED` ([ADR-0167](../../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md)
/// decision 10): the store's totals, then each drawer's own report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombinedReport {
    /// Each figure summed over the drawers below.
    pub totals: DrawerFigures,
    /// The drawers' tax by component and rate summed, in their order (ADR-0168 decision 5).
    pub tax_components: Vec<TaxComponentLine>,
    /// Each drawer's report, under its till's name, which the store's one drawer has none of.
    pub drawers: Vec<(Option<DisplayName>, ShiftReport)>,
}

/// Several counted drawers closed in one act (ADR-0167 decision 10).
#[derive(Debug, Clone)]
pub struct ClosedDrawers {
    /// Each drawer closed, in the order the request named them, as its own close would answer.
    pub shifts: Vec<ShiftView>,
    /// The one slip for all of them, where the store combines its close report.
    pub combined: Option<CombinedReport>,
}

/// What a cash shift looks like to a caller after a command.
///
/// The close is **blind** (§11.1): counting reveals nothing, so `expected_amount` and `variance` are
/// `None` on an open or counted shift and are populated only once the shift closes — the cashier
/// counts before the system says what it expected. A store that turns `shift.blind_close` off
/// ([ADR-0160](../../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
/// decision 2) is shown `expected_amount` before the close too, so the count can be made against
/// it; `variance` stays the close's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShiftView {
    /// The shift.
    pub shift_id: ShiftId,
    /// The till whose drawer it counts, or `None` for the store's one drawer
    /// ([ADR-0167](../../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 2).
    pub till: Option<DeviceId>,
    /// The person the drawer is assigned to for this session, where the store assigns drawers
    /// (decision 9): an id, never a name. `None` where drawers are shared.
    pub owner: Option<EmployeeId>,
    /// Its state after the command.
    pub state: ShiftState,
    /// What the system expected in the drawer: revealed at close, and before it only where the
    /// store's count is not blind.
    pub expected_amount: Option<Money>,
    /// What the cashier counted, once counted.
    pub counted_amount: Option<Money>,
    /// Counted minus expected, revealed only at close. Negative means short.
    pub variance: Option<Money>,
    /// Cash paid into the drawer outside a sale so far
    /// ([ADR-0165](../../../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md)).
    ///
    /// Not the expectation, and nothing the close keeps blind: the cashier entered every one of
    /// these, and the cash taken on bills, which is the part they do not know, is not here.
    pub paid_in: Money,
    /// Cash paid out of the drawer outside a sale so far.
    pub paid_out: Money,
    /// Whether closing asked for the shift report to print, for the caller to run after commit.
    pub print_shift_report: bool,
    /// The figures that report prints — present only on the close, which is the one command that
    /// has them all and the last moment the shift's record exists.
    pub report: Option<ShiftReport>,
    /// Whether the shift opened on an earlier business day than today's and is still open: its
    /// day has ended ([ADR-0167](../../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md)
    /// decision 11).
    pub day_ended: bool,
}

/// Which till a request comes from, and which till's drawer it names
/// ([ADR-0167](../../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decisions 2 and 3).
///
/// Read only where the edge keeps a drawer per till: where it keeps one, every device's drawer is
/// the store's, and both are ignored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TillScope {
    /// The till the device is, by its print-agent binding to a `TERMINAL` entry the store's
    /// `devices` node lists (ADR-0112); `None` for a device that is no till.
    pub own: Option<DeviceId>,
    /// The till whose drawer the request names, when it names one.
    pub named: Option<DeviceId>,
}

/// One drawer the store keeps, for `GET /api/shifts` (ADR-0167 decision 3).
#[derive(Debug, Clone)]
pub struct DrawerView {
    /// The till it belongs to, or `None` for the store's one drawer.
    pub till: Option<DeviceId>,
    /// The till's name, as its `TERMINAL` entry gives it.
    pub name: Option<DisplayName>,
    /// The float its shift opens with unless the person says otherwise: the till's own
    /// `opening_float_minor`, else the store's `shift.opening_float_minor`.
    pub default_float: Money,
    /// When its shift opened, while one is open.
    pub opened_time: Option<Timestamp>,
    /// Its shift, while one is open, as blind as any other read of it.
    pub shift: Option<ShiftView>,
}

/// Every drawer the store keeps, and the model it keeps them under (ADR-0167 decisions 1 and 8).
#[derive(Debug, Clone)]
pub struct DrawersView {
    /// The model the edge runs now.
    pub model: DrawerModel,
    /// The model the store has published, while open shifts keep the edge on the other until they
    /// close.
    pub waiting: Option<DrawerModel>,
    /// The till the device asking is, where the edge keeps a drawer per till: its own drawer, which
    /// the till's Shift screen shows first. `None` for a device that is no till, and where the edge
    /// keeps one drawer.
    pub own: Option<DeviceId>,
    /// The store's one drawer, or one per till its `devices` node lists.
    pub drawers: Vec<DrawerView>,
    /// Whether the store's drawers are anybody's or each one person's (ADR-0167 decision 9).
    pub access: DrawerAccess,
}

/// Which way cash moved through the drawer outside a sale
/// ([ADR-0165](../../../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md) decision 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CashMovement {
    /// Cash put in: change brought from the bank, a float topped up.
    PaidIn,
    /// Cash taken out: a supplier paid at the door, petty cash.
    PaidOut,
}

impl CashMovement {
    /// The act the store's reason list names for this movement (ADR-0115).
    pub(in crate::app) const fn reason_action(self) -> ReasonAction {
        match self {
            Self::PaidIn => ReasonAction::CashPaidIn,
            Self::PaidOut => ReasonAction::CashPaidOut,
        }
    }
}
