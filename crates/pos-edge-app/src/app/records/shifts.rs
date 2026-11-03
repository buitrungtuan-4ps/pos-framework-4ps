// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the projection remembers about cash shifts, and which drawer a settle's cash goes to.

use pos_core::billing::TaxComponentLine;
use pos_core::cash_count::CountBreakdown;
use pos_core::error::DomainError;
use pos_proto::devices::DeviceKind;
use pos_proto::ids::{DeviceId, EmployeeId, ShiftId};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::{PaymentMethod, ShiftState};

use crate::app::views::{ShiftReport, ShiftView, TillScope};

/// What the projection remembers about a cash shift: its state, the float it opened with, the cash
/// its settled bills have taken (the rollup the close compares against), the cash paid in and out
/// outside a sale, and the blind count once entered. Keeping the count here but never revealing the
/// expectation before close is what makes the close blind (§11.1).
#[derive(Debug, Clone)]
pub(in crate::app) struct ShiftRecord {
    /// The till whose drawer the shift counts, the `TERMINAL` entry's id, or `None` for the store's
    /// one drawer ([ADR-0167](../../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 2).
    pub(in crate::app) till: Option<DeviceId>,
    /// The person the drawer is assigned to for this session, as its `cash.shift.opened` records,
    /// where the store assigns drawers (decision 9); `None` where they are shared.
    pub(in crate::app) owner: Option<EmployeeId>,
    /// When it opened, which the drawers read reports.
    pub(in crate::app) opened_time: Timestamp,
    /// The business date it opened on: once today's is later, its day has ended (ADR-0167
    /// decision 11).
    pub(in crate::app) business_date: BusinessDate,
    /// The business date the edge last warned that the drawer is open past its day's end, so it
    /// warns once a day.
    pub(in crate::app) day_end_warned: Option<BusinessDate>,
    pub(in crate::app) state: ShiftState,
    pub(in crate::app) opening_float: Money,
    pub(in crate::app) cash_collected: Money,
    pub(in crate::app) paid_in: Money,
    pub(in crate::app) paid_out: Money,
    /// Cash taken to the safe (ADR-0186), out of the drawer like a paid out.
    pub(in crate::app) dropped: Money,
    /// The cash given back from this drawer by voiding bills that were paid
    /// ([ADR-0184](../../../../../docs/adr/0184-a-settled-bill-is-voided-whole-not-refunded.md)
    /// decision 6): each voided bill's cash, wherever it was taken, leaves the drawer of the till
    /// the void is done at.
    pub(in crate::app) voided_cash: Money,
    /// How many paid bills were voided at this drawer's till during the shift, cash or not
    /// (ADR-0184), for its close card.
    pub(in crate::app) voided_bills: u32,
    pub(in crate::app) counted: Option<Money>,
    /// How many bills it settled, for its report.
    pub(in crate::app) bills: u32,
    /// What its settled bills came to, what their guests paid, for its X (ADR-0187 decision 4).
    pub(in crate::app) sales: Money,
    /// The discounts and comps on those bills, for its X.
    pub(in crate::app) discounts: Money,
    /// What each tender applied to those bills, for its report's tender mix.
    pub(in crate::app) tenders: Vec<(PaymentMethod, Money)>,
    /// The count note by note, where it was counted so (ADR-0186); empty for one typed amount.
    pub(in crate::app) count_breakdown: CountBreakdown,
    /// The float note by note, where it was counted so; empty for one typed amount.
    pub(in crate::app) float_breakdown: CountBreakdown,
    /// The shift it took the drawer over from at a handover (ADR-0187), whose slip it reprints.
    pub(in crate::app) handed_over_from: Option<ShiftId>,
    /// The tax by component and rate its settled bills recorded on their tax lines (ADR-0168
    /// decision 5), one entry for each name and rate, and none for a line that recorded none.
    /// Fee tax is inside the tax lines, so a fee's own components are not added again.
    pub(in crate::app) tax_components: Vec<TaxComponentLine>,
}

impl ShiftRecord {
    /// A shift as it opens on `till`'s drawer, assigned to `owner`, at `opened_time`, on
    /// `business_date`: its float, and nothing taken, paid in or out, or counted yet, in `currency`.
    pub(in crate::app) fn opened(
        opening_float: Money,
        currency: CurrencyCode,
        (till, owner): (Option<DeviceId>, Option<EmployeeId>),
        opened_time: Timestamp,
        business_date: BusinessDate,
    ) -> Self {
        let zero = Money::zero(currency);
        Self {
            till,
            owner,
            opened_time,
            business_date,
            day_end_warned: None,
            state: ShiftState::Open,
            opening_float,
            cash_collected: zero,
            paid_in: zero,
            paid_out: zero,
            dropped: zero,
            voided_cash: zero,
            voided_bills: 0,
            counted: None,
            bills: 0,
            sales: zero,
            discounts: zero,
            tenders: Vec::new(),
            count_breakdown: CountBreakdown::default(),
            float_breakdown: CountBreakdown::default(),
            handed_over_from: None,
            tax_components: Vec::new(),
        }
    }

    /// The cash the drawer is expected to hold: the float, plus every cash payment taken on the
    /// shift, plus what was paid in, minus what was paid out (ADR-0165 decision 2), minus the cash
    /// given back by voiding paid bills here (ADR-0184 decision 6), minus what was dropped to the
    /// safe (ADR-0186). Cash rounding and non-cash
    /// tenders never touch the drawer, so they are not in it.
    pub(in crate::app) fn expected(&self) -> Result<Money, DomainError> {
        Ok(self
            .opening_float
            .checked_add(self.cash_collected)?
            .checked_add(self.paid_in)?
            .checked_sub(self.paid_out)?
            .checked_sub(self.dropped)?
            .checked_sub(self.voided_cash)?)
    }
}

/// `record`'s drawer closed, every figure revealed, with its report and whether to print it.
pub(in crate::app) fn closed_view(
    record: &ShiftRecord,
    report: ShiftReport,
    print_shift_report: bool,
) -> ShiftView {
    ShiftView {
        shift_id: report.shift_id,
        till: record.till,
        owner: record.owner,
        state: ShiftState::Closed,
        expected_amount: Some(report.expected_amount),
        counted_amount: Some(report.counted_amount),
        variance: Some(report.variance),
        paid_in: report.paid_in,
        paid_out: report.paid_out,
        dropped: report.dropped,
        print_shift_report,
        report: Some(report),
        day_ended: false,
    }
}

/// Where a settle's cash goes: the drawer, the shift its payments are stamped with, and the approver
/// of the override cash into a drawer assigned to somebody else took (ADR-0167 decisions 6 and 9).
pub(in crate::app) type SettleDrawer = (Option<DeviceId>, Option<ShiftId>, Option<EmployeeId>);

/// Whether `device` is a till, a `TERMINAL` entry on the store's `devices` node.
pub(in crate::app) fn is_till(device: &pos_proto::devices::PublishedDevice) -> bool {
    device.kind.known() == DeviceKind::Terminal
}

/// Whether a shift is another till's drawer's than the till `scope` says the device is
/// ([ADR-0167](../../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 3). The store's
/// one drawer is every till's, so it is never another's.
pub(in crate::app) fn another_till(scope: TillScope, record: &ShiftRecord) -> bool {
    record.till.is_some() && scope.own != record.till
}
