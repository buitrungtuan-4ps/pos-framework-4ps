// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a command can fail with, and the manager's approval a step-up carries.

use std::fmt;

use pos_core::error::DomainError;
use pos_core::menu::RepriceError;
use pos_ports::PortError;
use pos_proto::envelope::DecodeError;

use crate::app::views::ShiftView;

/// A failure applying a command.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The domain refused the command — a bad transition, a missing permission, a disabled capability.
    #[error("the command was refused: {0}")]
    Domain(#[from] DomainError),
    /// The store could not be read or written.
    #[error("the store failed: {0}")]
    Port(#[from] PortError),
    /// The business date could not be derived from the clock and the store's timezone.
    #[error("could not derive the business date")]
    Clock,
    /// An event payload could not be encoded to JSON.
    #[error("could not encode an event: {0}")]
    Encode(DecodeError),
    /// A line was added to a table that has no open order — it must be seated first.
    #[error("the table has no open order")]
    NoOpenOrder,
    /// A command named a line the edge does not know.
    #[error("no such order line")]
    UnknownLine,
    /// A line was fired but no station could be resolved — the store has no station plan (no rule
    /// matched and none is the default) and the caller named no station either (ADR-0072).
    #[error("the line routes to no station")]
    UnroutableLine,
    /// A command named an order the edge does not know — no floor order on a table, and no lines
    /// either, so there is nothing to bill.
    #[error("no such order")]
    UnknownOrder,
    /// A bill was opened on an order that already has one open (ADR-0093). Two bills on one order
    /// would take two receipt numbers and charge the guest twice for one meal.
    #[error("a bill is already open on that order")]
    BillAlreadyOpen,
    /// A table was released with something sold on it: a line not voided, or a bill open
    /// ([ADR-0163](../../../../docs/adr/0163-a-table-seated-by-mistake-is-released.md)). Releasing
    /// says nobody ate, so it can only end an order with nothing on it.
    #[error("that table has something sold on it; take payment or void the lines first")]
    OrderNotEmpty,
    /// A bill was asked for on an order with nothing to bill: no line, or every line voided
    /// (ADR-0163). It could never settle, and its table would wait for a payment that cannot come.
    #[error("that order has nothing to bill")]
    NothingToBill,
    /// A copy was asked of a bill that has not settled, or was voided since
    /// ([ADR-0164](../../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)):
    /// there is no receipt to copy. A bill still open prints its check as a pre-bill.
    #[error("that bill has no receipt to copy, because it has not settled")]
    NotSettled,
    /// A copy was asked of a receipt settled on an earlier business day (ADR-0164 decision 3).
    /// The till copies today's receipts; older ones are reprinted from the console.
    #[error("that receipt is from another business day")]
    ReceiptFromAnotherDay,
    /// A command named a bill the edge does not know.
    #[error("no such bill")]
    UnknownBill,
    /// A pre-bill was asked for where nothing is open to print one for: a free table, an order with
    /// nothing on it, or a bill already settled, voided, split or merged. A settled bill's paper is
    /// its receipt, not a pre-bill.
    #[error("nothing is open to print a pre-bill for")]
    NothingToPrint,
    /// A merge named bills that do not sit on the same table, or named the target as its own
    /// absorbed bill ([ADR-0128](../../../../docs/adr/0128-a-bill-splits-and-merges.md) decision 5).
    ///
    /// The model does not forbid a bill spanning two tables; the **floor** does. Settling moves a
    /// bill's table from `AwaitingPayment` to `NeedsCleaning`, and a bill over two tables makes
    /// "which table moved?" a question with two answers. Merging a bill into itself is refused here
    /// too, because it would double that bill's own lines — the one arithmetic mistake ADR-0128
    /// exists to make impossible.
    #[error("bills on different tables cannot be merged")]
    BillsOnDifferentTables,
    /// A merge named a bill of another order than the target's
    /// ([ADR-0128](../../../../docs/adr/0128-a-bill-splits-and-merges.md) decision 5).
    ///
    /// Nothing in the model stops one bill covering two orders' lines, but nothing settles such a
    /// bill: a settle reads a bill's lines from its own order, so the other order's would be charged
    /// nothing, and the bill that held them, merged, is terminal, so that order would never be paid.
    /// Two counter bills share a table, none, so this is what refuses them. Checked after the table,
    /// so bills on two tables keep [`Self::BillsOnDifferentTables`].
    #[error("bills on different orders cannot be merged")]
    BillsOnDifferentOrders,
    /// A command named a shift the edge does not know.
    #[error("no such shift")]
    UnknownShift,
    /// A shift was opened while one is already open — one open shift per device (§6, archive).
    #[error("a shift is already open")]
    ShiftAlreadyOpen,
    /// The store keeps a drawer per till, and the device acting is none of its tills, nor did the
    /// request name one ([ADR-0167](../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6).
    #[error("this device is not one of the store's tills; bind it to one, or name the till")]
    NotATill,
    /// A table was seated, a counter order started or a payment taken while no shift is open, at a
    /// store whose `shift` node refuses that
    /// ([ADR-0160](../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)).
    #[error("no shift is open; open a shift before selling")]
    OpenShiftRequired,
    /// A line was fired on a guest's order a member of staff has not yet confirmed
    /// ([ADR-0116](../../../../docs/adr/0116-the-qr-hold-is-derived-and-it-gates-firing.md)).
    ///
    /// The order is real and billable; what has not happened is anyone agreeing to make it. This is
    /// the guardrail ADR-0012 and ADR-0057 promised and nothing enforced.
    #[error("that order is waiting for a member of staff to confirm it")]
    AwaitingStaffConfirmation,
    /// A confirmation or a rejection named an order that is not waiting for one — it was never a
    /// held guest submission, or staff have already decided (ADR-0116).
    #[error("that order is not waiting for staff confirmation")]
    NotAwaitingStaffConfirmation,
    /// A rejection cited a reason the store's managed list does not hold for rejecting an order
    /// ([ADR-0115](../../../../docs/adr/0115-reason-codes-are-a-managed-list.md)).
    #[error("that reason code is not valid for rejecting an order")]
    ReasonCodeNotValid,
    /// A line was fired on an order a member of staff **refused** (ADR-0116).
    ///
    /// Distinct from [`AwaitingStaffConfirmation`](Self::AwaitingStaffConfirmation): that one is
    /// "nobody has decided yet" and ends when somebody does; this one is decided, and the answer
    /// was no. Collapsing the two into "has a decision been recorded" is exactly the bug the
    /// enforcement suite caught — a refused order's line became fireable the moment the refusal
    /// landed, which is the opposite of what a refusal means.
    #[error("that order was refused by a member of staff")]
    OrderRejected,
    /// A rejection named an order whose kitchen has already started. That is a void, with a void's
    /// permission and its own reason (roadmap B2.2), not a rejection.
    #[error("that order has already fired and can no longer be rejected")]
    AlreadyFired,
    /// A high-risk act needed a manager's PIN and none was offered (`docs/pos-spec.md` §9, §11.4).
    ///
    /// The permission registry marks the act `pin: true`, and until roadmap B2.2 **nothing read that
    /// flag**: `Grant::pin_required` was set, the domain checked it, and no caller ever passed a
    /// verified PIN — so the only act that could reach the check was one that never ran. This is the
    /// refusal that flag always described.
    #[error("that action needs a manager's PIN")]
    ApprovalRequired,
    /// A manager's PIN was offered and did not authorise the act — an unknown code, a wrong PIN, or
    /// a person whose role does not hold the permission being exercised.
    ///
    /// One refusal for all three on purpose: telling a caller which of them failed would let anyone
    /// at the till enumerate who can approve a void.
    #[error("that manager PIN does not authorise this action")]
    ApprovalRefused,
    /// A manager's PIN was offered for someone the PIN lockout is holding: too many wrong PINs in a
    /// row, at the sign-in screen or at an approval prompt (ADR-0030).
    ///
    /// Refused before the PIN is checked, so a right PIN does not get through until the lockout
    /// ends, and guessing faster does not help. It names no more than the sign-in screen already
    /// does, which answers "locked out" for the same code.
    #[error("that manager is locked out after too many wrong PINs; try again in a few minutes")]
    ApproverLockedOut,
    /// A command that would **open** a new order or a new shift was refused because a replacement
    /// machine has taken this store
    /// ([ADR-0123](../../../../docs/adr/0123-a-superseded-box-opens-nothing-new.md), closing
    /// [ADR-0049](../../../../docs/adr/0049-single-active-lease.md)'s edge half).
    ///
    /// Only the four commands that create an order or a shift raise it. Everything already open on
    /// this box still fires, bills, voids and settles — the refusal drains the floor, it does not
    /// stop the shop taking money for work it has already done.
    #[error(
        "this machine has been replaced; finish and settle what is open here, and take new orders \
         on the replacement"
    )]
    Superseded,
    /// A void cited a reason the store's managed list does not hold for voiding
    /// ([ADR-0115](../../../../docs/adr/0115-reason-codes-are-a-managed-list.md)).
    ///
    /// Separate from [`ReasonCodeNotValid`](Self::ReasonCodeNotValid), which is a rejection's: the
    /// two actions draw from different entries of the same list, and a message naming the wrong act
    /// would send an operator to the wrong row.
    #[error("that reason code is not valid for voiding")]
    VoidReasonNotValid,
    /// A paid in, a paid out, a no-sale opening or a close's over or short cited a reason the store's
    /// managed list does not hold for that act ([ADR-0165](../../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md),
    /// [ADR-0167](../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 12).
    ///
    /// One refusal for the four: they are all the drawer's acts, and the till offers each only the
    /// reasons listed for it, so a message naming the drawer sends an operator to the right rows.
    #[error("that reason code is not valid for this act on the drawer")]
    CashReasonNotValid,
    /// A close of a counted drawer over or short by more than the store's
    /// `shift.variance_reason_minor`, while the store's list offers a reason for that, without one
    /// ([ADR-0167](../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 12).
    ///
    /// Carries each such drawer as its close would show it. Its count is final, so these are the
    /// figures the close reveals, shown to the person closing it, where the till asks the reason.
    #[error("a drawer's over or short is beyond the store's limit; give a reason to close it")]
    VarianceReasonRequired(Vec<ShiftView>),
    /// A paid in or a paid out named a shift that is not the one trading, or one already counted
    /// ([ADR-0165](../../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md) decision 1).
    ///
    /// A counted shift is refused because its blind count was made against the drawer as it stood,
    /// and a movement after the count would change what the count is checked against.
    #[error("that shift is not open for cash to be paid in or out")]
    ShiftNotOpen,
    /// Cash into a drawer whose shift opened on an earlier business day, at a store that requires
    /// such a drawer counted and closed first
    /// ([ADR-0167](../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 11).
    #[error("this drawer's shift opened on an earlier business day; count and close it first")]
    DrawerDayEnded,
    /// An act on a drawer assigned, for its session, to somebody other than the person acting, who
    /// neither holds `cash.drawer.override_assignment` directly nor brought an approver who does
    /// ([ADR-0167](../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 9). Names no
    /// person.
    #[error("this drawer is assigned to someone else; a manager can approve this one act")]
    DrawerAssignedToAnother,
    /// Another till's drawer started where the store assigns drawers, naming nobody on the store's
    /// roster to assign it to (ADR-0167 decision 9).
    #[error("name the person on the store's staff this drawer is for")]
    AssigneeRequired,

    /// The modifiers a line names break a group's selection rule, or belong to no group the item
    /// attaches ([ADR-0127](../../../../docs/adr/0127-modifier-groups-reach-the-edge.md)).
    ///
    /// The edge checks this rather than trusting the device to have asked. A required choice a till
    /// that forgot to ask can skip is required in name only, and the money is already wrong by the
    /// time the kitchen reads the ticket — the line was priced without the size it was sold at.
    #[error("the modifiers on that line break the item's selection rules")]
    ModifierSelectionInvalid,
    /// The store does not accept orders on that sales channel (ADR-0146): the counter asked to open
    /// an order on a channel its published `channels` node leaves out.
    #[error("this store does not accept orders on that sales channel")]
    ChannelNotAccepted,
    /// The item, or one of its modifiers, is not in the store's price book or is 86'd right now,
    /// so the edge cannot price the line (ADR-0146). Refused, never substituted.
    #[error("that item cannot be sold right now")]
    ItemNotSellable,
}

impl AppError {
    /// Why the price book could not price a line, as a refusal (ADR-0146). A missing rate is the
    /// same configuration fault the bill reports, so it keeps that token.
    pub(in crate::app) fn from_reprice(error: RepriceError) -> Self {
        match error {
            RepriceError::MissingRate {
                tax_class_id,
                sales_channel,
            } => Self::Domain(DomainError::TaxRateNotConfigured {
                tax_class_id: tax_class_id.to_string(),
                sales_channel: pos_proto::WireEnum::as_wire(sales_channel).to_owned(),
            }),
            RepriceError::Money(error) => Self::Domain(DomainError::Money(error)),
            // An unknown or 86'd item, and — `RepriceError` being non-exhaustive — any reason this
            // build has not learned: the line cannot be priced, and a refusal is the safe answer.
            _ => Self::ItemNotSellable,
        }
    }
}

/// A manager's step-up authorisation for one act (`docs/pos-spec.md` §9, §11.4).
///
/// The framework's high-risk permissions carry `pin: true`: a colleague who lacks one calls someone
/// who holds it to tap their badge code and PIN on the same device. This is that pair, and it
/// authorises **exactly one act** — it is not a session, it grants nothing else, and it is never
/// stored.
///
/// # Who the log blames
///
/// The envelope's actor stays the person who performed the act, and the approver is named on a
/// companion `security.permission.overridden`. That split is the event catalogue's own — its
/// `approver_employee_id` field is documented as "Who authorised it. The employee who *requested* it
/// is on the envelope." A trail that recorded only the manager would hide who voided; one that
/// recorded only the server would hide who let them.
#[derive(Clone)]
pub struct Approval {
    /// The approver's badge code.
    pub code: String,
    /// The approver's PIN. Verified against the published Argon2id hash and never stored.
    pub pin: String,
}

impl fmt::Debug for Approval {
    /// Redacts both halves. The PIN because a `Debug` that printed it would put a manager's
    /// credential into any log line that formatted the surrounding request — and, since ADR-0117,
    /// the badge **code** too: it names which manager authorised what, and a durable line carrying
    /// it is a managerial-activity record outside `SubjectStore`. The approver reaches the event
    /// catalogue as `approver_employee_id` on `security.permission.overridden`, which is the place
    /// that question is meant to be answered from.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Approval")
            .field("code", &"<redacted>")
            .field("pin", &"<redacted>")
            .finish()
    }
}
