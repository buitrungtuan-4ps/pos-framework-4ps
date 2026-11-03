// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a shift's X says: how it stands now, read at the till, resetting nothing
//! ([ADR-0187](../../../../../docs/adr/0187-a-shift-is-handed-over-at-the-till-and-read-there.md)
//! decision 4).

use pos_core::error::DomainError;
use pos_proto::ShiftState;
use pos_proto::ids::{DeviceId, ShiftId};
use pos_proto::money::Money;
use pos_proto::text::DisplayName;
use pos_proto::time::{BusinessDate, Timestamp};

/// What each tender took, in four kinds: cash, card, a scan-to-pay wallet, and every other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TenderTotals {
    /// Cash.
    pub cash: Money,
    /// Card.
    pub card: Money,
    /// A scan-to-pay wallet.
    pub qr: Money,
    /// Every other tender.
    pub other: Money,
}

/// One shift's figures, or several shifts' together, as an X reads them. Names no person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XFigures {
    /// What the shift's settled bills came to: what their guests paid.
    pub sales: Money,
    /// How many bills it settled.
    pub bills: u32,
    /// What each tender applied to those bills, change and tips apart.
    pub tenders: TenderTotals,
    /// The discounts and comps on those bills.
    pub discounts: Money,
    /// How many paid bills were voided at the drawer's till during the shift (ADR-0184).
    pub voided_bills: u32,
    /// The cash those voids handed back out of the drawer.
    pub voided_cash: Money,
    /// Cash paid in outside a sale.
    pub paid_in: Money,
    /// Cash paid out outside a sale.
    pub paid_out: Money,
    /// Cash dropped to the safe (ADR-0186).
    pub dropped: Money,
    /// The cash the drawer should hold now, or held at its close: `None` where the store counts
    /// blind and the reader does not hold `reports.shift.read_x`.
    pub expected_cash: Option<Money>,
}

impl XFigures {
    /// `self` and `other` together; the expectation only where both show one.
    ///
    /// # Errors
    ///
    /// [`DomainError`] where a sum overflows or the currencies differ.
    pub fn plus(&self, other: &Self) -> Result<Self, DomainError> {
        let expected_cash = match (self.expected_cash, other.expected_cash) {
            (Some(left), Some(right)) => Some(left.checked_add(right)?),
            _ => None,
        };
        Ok(Self {
            sales: self.sales.checked_add(other.sales)?,
            bills: self.bills.saturating_add(other.bills),
            tenders: TenderTotals {
                cash: self.tenders.cash.checked_add(other.tenders.cash)?,
                card: self.tenders.card.checked_add(other.tenders.card)?,
                qr: self.tenders.qr.checked_add(other.tenders.qr)?,
                other: self.tenders.other.checked_add(other.tenders.other)?,
            },
            discounts: self.discounts.checked_add(other.discounts)?,
            voided_bills: self.voided_bills.saturating_add(other.voided_bills),
            voided_cash: self.voided_cash.checked_add(other.voided_cash)?,
            paid_in: self.paid_in.checked_add(other.paid_in)?,
            paid_out: self.paid_out.checked_add(other.paid_out)?,
            dropped: self.dropped.checked_add(other.dropped)?,
            expected_cash,
        })
    }
}

/// One shift's X: which drawer, its state, and its figures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShiftX {
    /// The shift.
    pub shift_id: ShiftId,
    /// The till whose drawer it is, `None` for the store's one drawer.
    pub till: Option<DeviceId>,
    /// That till's name, as its `TERMINAL` entry gives it.
    pub till_name: Option<DisplayName>,
    /// Open, counted, or closed.
    pub state: ShiftState,
    /// The business date it opened on.
    pub business_date: BusinessDate,
    /// When it opened.
    pub opened_time: Timestamp,
    /// Its figures.
    pub figures: XFigures,
}

/// The store's X across its drawers: every open drawer's shift, or, once none is open, the
/// business date's closed shifts, which is the store's end of day (decision 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreX {
    /// The business date it reads.
    pub business_date: BusinessDate,
    /// Whether no drawer is open and the day's shifts all closed: "All tills closed for the day".
    pub all_closed: bool,
    /// Each shift, oldest first.
    pub shifts: Vec<ShiftX>,
    /// Their figures together, `None` where there is no shift to read.
    pub totals: Option<XFigures>,
}
