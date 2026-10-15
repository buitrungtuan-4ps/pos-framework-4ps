// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the edge answers about bills, receipts, the takings and the buyer of a corporate invoice.

use std::collections::BTreeMap;
use std::fmt;

use pos_core::billing::BillTotals;
use pos_ports::subject_store::SubjectRecord;
use pos_proto::ids::{BillId, DeviceId, MenuItemId, OrderId, OrderLineId, SubjectId, TableId};
use pos_proto::money::Money;
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::{BillState, TableState};

/// One row of a receipt: what was sold, how many, at what each, for how much
/// ([ADR-0129](../../../../../docs/adr/0129-a-receipt-itemises-what-was-sold.md)).
///
/// The name and the price are the **snapshot** `sales.order_line.added` captured, not what the menu
/// says now — a settled bill does not change because somebody renamed an item next week (§14.2).
/// That is the one way this differs from [`CounterOrderLine`](crate::app::CounterOrderLine), which resolves against the live menu
/// on purpose, because a screen showing an open order wants today's spelling.
///
/// Edge-local, like [`BillTotals`] beside it: it crosses no wire and is not part of any response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptLine {
    /// The item sold, by which a receipt in another language finds the menu's name for it.
    pub menu_item_id: MenuItemId,
    /// The item's name as the guest saw it when the line was added.
    pub display_name: DisplayName,
    /// How many — a [`Quantity`], because a split item is half of one and a weighed item is not a
    /// whole number either.
    pub quantity: Quantity,
    /// The price of one, as captured. Any modifier's price is already inside it.
    pub unit_price: Money,
    /// The extended amount for the row. The rows sum to the bill's `subtotal`.
    pub line_total: Money,
    /// The modifiers' names as the guest saw them when the line was added, printed `  + name` under
    /// the row ([ADR-0144](../../../../../docs/adr/0144-a-line-records-the-names-of-its-modifiers.md)).
    ///
    /// Empty for a line recorded before `sales.order_line.added` carried them: such a receipt
    /// prints neither an id nor a name looked up today, so an old receipt reprints as it was.
    pub modifier_display_names: Vec<DisplayName>,
    /// The modifiers chosen, in the order of [`Self::modifier_display_names`].
    pub modifier_menu_item_ids: Vec<MenuItemId>,
}

/// What a pre-bill prints: the rows a bill or an open order covers, and what they come to.
///
/// The same two things a receipt is made of, read through the same projection and the same
/// [`billing::assemble`](pos_core::billing::assemble) the settle runs, so a table that pays exactly the pre-bill's figure pays
/// what the bill settles against.
#[derive(Debug, Clone, PartialEq)]
pub struct PreBill {
    /// The rows, as the receipt will print them.
    pub lines: Vec<ReceiptLine>,
    /// What they come to.
    pub totals: BillTotals,
}

/// The buyer a corporate invoice is issued to, as the cashier typed it
/// ([ADR-0107](../../../../../docs/adr/0107-the-buyer-is-a-subject.md)).
///
/// Personal data, every field of it. It never enters an event — `pos_proto::pii` makes that a
/// compile error — and [`Edge::settle_bill`](crate::app::Edge::settle_bill) files it in the store's subject store under a freshly
/// minted [`SubjectId`], which is what the settled event carries instead.
///
/// The tax code is here rather than treated as business data because for a sole trader it is not:
/// Japan's 登録番号 for a sole proprietor is issued to the individual, and an Indian GSTIN embeds a
/// PAN. A framework that got that wrong would be wrong in the direction that cannot be undone.
#[derive(Clone, PartialEq, Eq)]
pub struct BuyerDetails {
    /// The buyer's name, as it must appear on the invoice.
    pub name: String,
    /// Their registration number — a 登録番号, a GSTIN, an MST. Format-checked by the country
    /// module at the route, never checked for existence: that is a call to the authority, and a
    /// cashier must be able to take a corporate customer's number with the line down.
    pub tax_code: Option<String>,
    /// Their address, which India's Rule 46 requires and Japan's qualified invoice does not.
    pub address: Option<String>,
    /// Where to send a copy, when the store offers one.
    pub email: Option<String>,
}

/// Deliberately hand-written: `AGENTS.md` §2 forbids personal data in logs, and a derived `Debug`
/// would put a buyer's name and address into any span that carried this — including the request
/// span of the settle itself.
impl fmt::Debug for BuyerDetails {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BuyerDetails").finish_non_exhaustive()
    }
}

impl BuyerDetails {
    /// The fields as the subject store holds them: a map of name to value, with the absent ones
    /// left out rather than stored empty.
    ///
    /// Absent rather than empty because masking keeps the **keys** — they are the record of what
    /// kind of data was held — so writing an empty `address` would leave a store claiming it once
    /// held an address it never had.
    #[must_use]
    pub fn fields(&self) -> BTreeMap<String, String> {
        let mut fields = BTreeMap::new();
        fields.insert("name".to_owned(), self.name.clone());
        for (key, value) in [
            ("tax_code", self.tax_code.as_ref()),
            ("address", self.address.as_ref()),
            ("email", self.email.as_ref()),
        ] {
            if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
                fields.insert(key.to_owned(), value.clone());
            }
        }
        fields
    }

    /// The buyer back from the subject store, the inverse of [`Self::fields`]: what a receipt
    /// copy prints again (ADR-0164). `None` for a record with no name left in it.
    #[must_use]
    pub fn from_record(record: &SubjectRecord) -> Option<Self> {
        let field = |key: &str| {
            record
                .fields
                .get(key)
                .filter(|value| !value.trim().is_empty())
                .cloned()
        };
        Some(Self {
            name: field("name")?,
            tax_code: field("tax_code"),
            address: field("address"),
            email: field("email"),
        })
    }
}

/// A bill this store settled today, as the till's list of recent bills shows it
/// ([ADR-0164](../../../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledBillView {
    /// The bill.
    pub bill_id: BillId,
    /// Its order, which a counter's queue number is kept against.
    pub order_id: OrderId,
    /// Its table, or `None` for a counter order.
    pub table_id: Option<TableId>,
    /// The receipt's number.
    pub receipt_number: u64,
    /// What the guest paid.
    pub total_due: Money,
    /// When it settled.
    pub settle_time: Timestamp,
    /// How many copies have been printed.
    pub copies: u32,
}

/// What this store has taken in one business day: the sum of its settled bills and how many there
/// were, for the Today screen's tile
/// ([ADR-0160](../../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
/// decision 2).
///
/// The store's figure and nothing finer: no person, no till and no shift, because takings per person
/// would be monitoring staff, which needs a legal basis this read does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Takings {
    /// The business day the figure is for.
    pub business_date: BusinessDate,
    /// What the day's settled bills came to, each at its `total_due`: what its guests paid.
    pub takings_amount: Money,
    /// How many bills settled that day.
    pub bill_count: u32,
}

/// A copy of a settled bill's receipt, counted and ready to print (ADR-0164).
///
/// No `PartialEq`: it carries the print job's id, which is fresh for every copy.
#[derive(Debug, Clone)]
pub struct ReceiptCopy {
    /// The bill.
    pub bill_id: BillId,
    /// The receipt's own number: a copy never takes a new one.
    pub receipt_number: u64,
    /// Which copy this is, counting from 1.
    pub copy_number: u32,
    /// When it was asked for, which the copy prints under its heading.
    pub reprinted_time: Timestamp,
    /// The rows the receipt printed.
    pub lines: Vec<ReceiptLine>,
    /// The totals, as the settle recorded them (`SettledReceipt::totals`).
    pub totals: BillTotals,
    /// The corporate buyer, whose details the copy prints again from the subject store.
    pub buyer_subject_id: Option<SubjectId>,
    /// The copy's own event id: its print job's idempotency key, so a copy retried after an
    /// ambiguous failure prints once, and the next press prints another.
    pub job_id: pos_proto::ids::EventId,
}

/// How many of today's settled bills the till's list shows, newest first (ADR-0164). A bound, as
/// every in-memory list has one, and more than a busy day's service at a till.
pub const MAX_SETTLED_LISTED: usize = 200;

/// What a bill looks like to a caller after a command.
///
/// After a settle it carries the gapless receipt number and the total the guest paid, plus the
/// state the bill and its table moved to. `print_receipt` reflects the [`Effect::PrintReceipt`](pos_core::decision::Effect::PrintReceipt)
/// the domain returned: the edge does not itself hold a printer, so the caller (the binary) runs the
/// effect after this returns — a rolled-back settle therefore never prints.
// Not `Copy` since ADR-0106: the settled totals carry per-rate tax lines, and a country deciding how
// many lines its invoice has is exactly the sort of thing a fixed-size type cannot hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillView {
    /// The bill.
    pub bill_id: BillId,
    /// Its state after the command.
    pub state: BillState,
    /// The gapless per-store receipt number, present once the bill has settled. Never a legal
    /// invoice number (ADR-0025).
    pub receipt_number: Option<u64>,
    /// What the guest owed, present once settled.
    pub total_due: Option<Money>,
    /// The settled bill's totals in full, present once settled — what the receipt is composed from
    /// ([ADR-0106](../../../../../docs/adr/0106-the-store-is-a-legal-person.md)).
    ///
    /// `total_due` above is the same figure and stays, because the till and the response shape read
    /// it and neither needs the tax lines. This carries what a *legal* document needs and the till
    /// does not: the per-rate tax, its named parts, and the rounding adjustment.
    ///
    /// Edge-local: it crosses no wire and is not part of the settle response.
    pub totals: Option<BillTotals>,
    /// The state the bill's table moved to as a result (P5 derives the floor cycle from the bill),
    /// or `None` for a counter order, which has no table to move (ADR-0093).
    pub table_state: Option<TableState>,
    /// Whether the settle asked for a receipt to be printed, for the caller to run after commit.
    pub print_receipt: bool,
    /// Whether the settle took cash, so the caller opens the cash drawer after commit
    /// ([ADR-0165](../../../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md) decision 4). Cash is what the drawer holds: a card, a QR payment or a
    /// voucher does not open it.
    pub open_drawer: bool,
    /// The till whose drawer the cash went into, where the store keeps a drawer per till, and
    /// `None` for the store's one drawer: the drawer the caller opens
    /// ([ADR-0167](../../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6).
    pub drawer_till: Option<DeviceId>,
    /// The rows that receipt lists, present once settled and empty otherwise (ADR-0129).
    ///
    /// Carried out of the settle rather than read back afterwards: the receipt is composed **after**
    /// the transaction commits (ADR-0100), by which point the projection has moved on, and these
    /// have to be the same records the totals above were assembled from or the document does not add
    /// up. Edge-local, like [`Self::totals`].
    pub lines: Vec<ReceiptLine>,
}

/// One bill as the till reads it back: where it has got to, the lines it covers, and what they come
/// to ([ADR-0128](../../../../../docs/adr/0128-a-bill-splits-and-merges.md)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillCheckView {
    /// Open while it still owes. A settled, voided, split or merged bill owes nothing more, and its
    /// totals are then what its lines came to rather than a sum to collect.
    pub state: BillState,
    /// The order lines it bills — how a till shows a guest what their part of a split table is for.
    pub order_line_ids: Vec<OrderLineId>,
    /// What those lines come to, less what has been taken off this bill.
    pub totals: BillTotals,
}
