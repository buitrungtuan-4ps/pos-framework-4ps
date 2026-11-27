// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a shift's X says: how it stands now, read at the till, resetting nothing
//! ([ADR-0187](../../../../../docs/adr/0187-a-shift-is-handed-over-at-the-till-and-read-there.md)
//! decision 4).

use pos_core::error::DomainError;
use pos_proto::ShiftState;
use pos_proto::WireEnum as _;
use pos_proto::ids::{DeviceId, ShiftId, VendorId};
use pos_proto::money::Money;
use pos_proto::sale_channel::SaleChannel;
use pos_proto::text::DisplayName;
use pos_proto::time::{BusinessDate, Timestamp};

/// What one delivery app's in-app payments applied
/// ([ADR-0259](../../../../../docs/adr/0259-a-sale-is-reported-by-its-channel-and-its-order-type.md)
/// decision 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InAppTotal {
    /// The app's connection.
    pub vendor_id: VendorId,
    /// What the store's `vendors` node calls it, `None` for an app it no longer lists.
    pub name: Option<DisplayName>,
    /// What its payments applied.
    pub amount: Money,
}

/// One line of the settled bills by channel and order type (ADR-0259 decision 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelLine {
    /// Where the orders came from, the app's connection included, and how they were served.
    pub channel: SaleChannel,
    /// The app's name for an app's line, as [`InAppTotal::name`].
    pub name: Option<DisplayName>,
    /// How many bills.
    pub bills: u32,
    /// What they came to.
    pub sales: Money,
}

/// Puts X lines in the order the console's X/Z lists them: by source token, app and type token.
pub(in crate::app) fn sort_lines(lines: &mut [ChannelLine]) {
    lines.sort_by(|left, right| {
        let key = |line: &ChannelLine| {
            (
                line.channel.source.as_wire(),
                line.channel.vendor_id,
                line.channel.order_type.as_wire(),
            )
        };
        key(left).cmp(&key(right))
    });
}

/// What each tender took, in four kinds: cash, card, a scan-to-pay wallet, and every other; and,
/// within every other, each delivery app's in-app payments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenderTotals {
    /// Cash.
    pub cash: Money,
    /// Card.
    pub card: Money,
    /// A scan-to-pay wallet.
    pub qr: Money,
    /// Every other tender, in-app payments included.
    pub other: Money,
    /// Each delivery app's in-app payments, in order of connection id (ADR-0259 decision 5).
    pub in_app: Vec<InAppTotal>,
}

/// One shift's figures, or several shifts' together, as an X reads them. Names no person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XFigures {
    /// What the shift's settled bills came to: what their guests paid. A bill voided after payment
    /// while the shift is open is left out (ADR-0258), and counted under `voided_bills`.
    pub sales: Money,
    /// How many bills it settled and still stand.
    pub bills: u32,
    /// What each tender applied to those bills, change and tips apart; a voided bill's payments are
    /// left out while the shift that took them is open (ADR-0258).
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
    /// The settled bills by channel and order type, in order of source, app and type; they sum to
    /// `bills` and `sales` (ADR-0259 decision 4).
    pub channels: Vec<ChannelLine>,
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
        let mut in_app = self.tenders.in_app.clone();
        for app in &other.tenders.in_app {
            match in_app
                .iter_mut()
                .find(|kept| kept.vendor_id == app.vendor_id)
            {
                Some(kept) => kept.amount = kept.amount.checked_add(app.amount)?,
                None => in_app.push(app.clone()),
            }
        }
        in_app.sort_by_key(|app| app.vendor_id);
        let mut channels = self.channels.clone();
        for line in &other.channels {
            match channels
                .iter_mut()
                .find(|kept| kept.channel == line.channel)
            {
                Some(kept) => {
                    kept.bills = kept.bills.saturating_add(line.bills);
                    kept.sales = kept.sales.checked_add(line.sales)?;
                }
                None => channels.push(line.clone()),
            }
        }
        sort_lines(&mut channels);
        Ok(Self {
            sales: self.sales.checked_add(other.sales)?,
            bills: self.bills.saturating_add(other.bills),
            tenders: TenderTotals {
                cash: self.tenders.cash.checked_add(other.tenders.cash)?,
                card: self.tenders.card.checked_add(other.tenders.card)?,
                qr: self.tenders.qr.checked_add(other.tenders.qr)?,
                other: self.tenders.other.checked_add(other.tenders.other)?,
                in_app,
            },
            discounts: self.discounts.checked_add(other.discounts)?,
            voided_bills: self.voided_bills.saturating_add(other.voided_bills),
            voided_cash: self.voided_cash.checked_add(other.voided_cash)?,
            paid_in: self.paid_in.checked_add(other.paid_in)?,
            paid_out: self.paid_out.checked_add(other.paid_out)?,
            dropped: self.dropped.checked_add(other.dropped)?,
            expected_cash,
            channels,
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
