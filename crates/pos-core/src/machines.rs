// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The five lifecycles `docs/architecture.md` §5 requires: Table, Order, order line, Bill, Shift.
//!
//! Each is a zero-size type implementing [`StateMachine`](crate::state_machine::StateMachine). The
//! states come from `pos-proto`'s wire enums, so the same tokens appear on the wire, in the
//! database, and here; the triggers are internal, because a trigger is a domain action rather than
//! something that crosses a boundary.
//!
//! The transition tables are deliberately small and explicit. Everything a reader needs to check
//! them against the specification is in one `match` per machine, and everything that proves them
//! sound is the one generic checker in [`crate::state_machine`].
//!
//! # Two subtleties worth stating
//!
//! **A settled bill is terminal, and a refund is not a transition out of it.** Bill has `Open`,
//! `Settled` and `Voided`, and both `Settled` and `Voided` are terminal — reached only from `Open`.
//! A post-settlement void or refund is a *new, signed movement* against the settled bill
//! ([ADR-0028](../../../docs/adr/0028-settlement-and-payment-invariant.md)), its own event, not a
//! `Settled → Voided` edge. Modelling it as an edge would make `Settled` non-terminal and reopen the
//! exclusive-settlement hole.
//!
//! **A table has no terminal state.** It cycles `Free → Occupied → AwaitingPayment → NeedsCleaning →
//! Free` forever, so the liveness check that every non-terminal reaches a terminal is skipped for it
//! — a cyclic lifecycle is not a deadlock.

use pos_proto::{BillState, OrderLineState, OrderState, ShiftState, TableState};

use crate::state_machine::StateMachine;

/// Declares a trigger enum together with its enumeration and its labels in one place, so the list a
/// machine iterates and the names it prints cannot drift apart.
macro_rules! triggers {
    (
        $(#[$enum_meta:meta])*
        $name:ident { $( $(#[$variant_meta:meta])* $variant:ident => $label:literal ),+ $(,)? }
    ) => {
        $(#[$enum_meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $( $(#[$variant_meta])* $variant, )+
        }

        impl $name {
            /// Every trigger, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The trigger's name, for the generated table and for a transition error.
            #[must_use]
            pub const fn label(self) -> &'static str {
                match self { $(Self::$variant => $label),+ }
            }
        }
    };
}

// -----------------------------------------------------------------------------------------------
// Table
// -----------------------------------------------------------------------------------------------

triggers! {
    /// What moves a table around the floor-plan cycle.
    TableTrigger {
        /// Seat guests at a free table.
        Seat => "seat",
        /// Guests ask for the bill.
        RequestBill => "request_bill",
        /// Payment completes and the guests leave.
        Settle => "settle",
        /// The table is cleaned and returned to service.
        Clean => "clean",
    }
}

/// The floor-plan lifecycle of a table.
///
/// `Free → Occupied → AwaitingPayment → NeedsCleaning → Free`. No terminal state: a table is reused.
/// The invariant that a table holds exactly one open order at a time is enforced by the entity
/// layer, not this machine — this machine only governs the table's own state.
#[derive(Debug, Clone, Copy)]
pub struct Table;

impl StateMachine for Table {
    type State = TableState;
    type Trigger = TableTrigger;
    const NAME: &'static str = "table";

    fn initial() -> TableState {
        TableState::Free
    }

    fn triggers() -> &'static [TableTrigger] {
        TableTrigger::ALL
    }

    fn trigger_name(trigger: TableTrigger) -> &'static str {
        trigger.label()
    }

    fn next(from: TableState, trigger: TableTrigger) -> Option<TableState> {
        Some(match (from, trigger) {
            (TableState::Free, TableTrigger::Seat) => TableState::Occupied,
            (TableState::Occupied, TableTrigger::RequestBill) => TableState::AwaitingPayment,
            (TableState::AwaitingPayment, TableTrigger::Settle) => TableState::NeedsCleaning,
            (TableState::NeedsCleaning, TableTrigger::Clean) => TableState::Free,
            _ => return None,
        })
    }

    fn is_terminal(_state: TableState) -> bool {
        false
    }

    fn rank(state: TableState) -> u8 {
        match state {
            TableState::Free => 0,
            TableState::Occupied => 1,
            TableState::AwaitingPayment => 2,
            TableState::NeedsCleaning => 3,
            TableState::Unspecified => u8::MAX,
        }
    }
}

// -----------------------------------------------------------------------------------------------
// Order
// -----------------------------------------------------------------------------------------------

triggers! {
    /// What ends an order's life.
    OrderTrigger {
        /// The order is paid in full.
        Settle => "settle",
        /// The order is cancelled in its entirety.
        Void => "void",
    }
}

/// The lifecycle of an order: `Open`, then terminal `Settled` or `Voided`.
#[derive(Debug, Clone, Copy)]
pub struct Order;

impl StateMachine for Order {
    type State = OrderState;
    type Trigger = OrderTrigger;
    const NAME: &'static str = "order";

    fn initial() -> OrderState {
        OrderState::Open
    }

    fn triggers() -> &'static [OrderTrigger] {
        OrderTrigger::ALL
    }

    fn trigger_name(trigger: OrderTrigger) -> &'static str {
        trigger.label()
    }

    fn next(from: OrderState, trigger: OrderTrigger) -> Option<OrderState> {
        Some(match (from, trigger) {
            (OrderState::Open, OrderTrigger::Settle) => OrderState::Settled,
            (OrderState::Open, OrderTrigger::Void) => OrderState::Voided,
            _ => return None,
        })
    }

    fn is_terminal(state: OrderState) -> bool {
        matches!(state, OrderState::Settled | OrderState::Voided)
    }

    fn rank(state: OrderState) -> u8 {
        match state {
            OrderState::Open => 0,
            OrderState::Settled => 1,
            OrderState::Voided => 2,
            OrderState::Unspecified => u8::MAX,
        }
    }
}

// -----------------------------------------------------------------------------------------------
// Order line
// -----------------------------------------------------------------------------------------------

triggers! {
    /// What moves a line through its life. `Fire` consumes stock; `Void` is terminal and, after a
    /// fire, records waste rather than returning stock.
    LineTrigger {
        /// Withhold a line from the kitchen.
        Hold => "hold",
        /// Return a held line to the editable set.
        Resume => "resume",
        /// Send the line to the kitchen; stock is consumed.
        Fire => "fire",
        /// Cancel the line, with a reason and a permission once fired.
        Void => "void",
    }
}

/// The lifecycle of one line on an order.
///
/// `Added` and `Held` are freely editable; `Fired` has consumed stock; `Voided` is terminal. This is
/// the machine whose merge matters most: two devices editing one line converge because `Voided`
/// outranks every editable state, so a void is never resurrected
/// ([ADR-0029](../../../docs/adr/0029-append-command-merge-semantics.md)).
#[derive(Debug, Clone, Copy)]
pub struct OrderLine;

impl StateMachine for OrderLine {
    type State = OrderLineState;
    type Trigger = LineTrigger;
    const NAME: &'static str = "order_line";

    fn initial() -> OrderLineState {
        OrderLineState::Added
    }

    fn triggers() -> &'static [LineTrigger] {
        LineTrigger::ALL
    }

    fn trigger_name(trigger: LineTrigger) -> &'static str {
        trigger.label()
    }

    fn next(from: OrderLineState, trigger: LineTrigger) -> Option<OrderLineState> {
        Some(match (from, trigger) {
            (OrderLineState::Added, LineTrigger::Hold) => OrderLineState::Held,
            (OrderLineState::Held, LineTrigger::Resume) => OrderLineState::Added,
            (OrderLineState::Added | OrderLineState::Held, LineTrigger::Fire) => {
                OrderLineState::Fired
            }
            (
                OrderLineState::Added | OrderLineState::Held | OrderLineState::Fired,
                LineTrigger::Void,
            ) => OrderLineState::Voided,
            _ => return None,
        })
    }

    fn is_terminal(state: OrderLineState) -> bool {
        matches!(state, OrderLineState::Voided)
    }

    fn rank(state: OrderLineState) -> u8 {
        match state {
            OrderLineState::Added => 0,
            OrderLineState::Held => 1,
            OrderLineState::Fired => 2,
            OrderLineState::Voided => 3,
            OrderLineState::Unspecified => u8::MAX,
        }
    }
}

// -----------------------------------------------------------------------------------------------
// Bill
// -----------------------------------------------------------------------------------------------

triggers! {
    /// What ends a bill's life. There is no post-settlement transition: a refund is a separate,
    /// signed event, not an edge out of `Settled`
    /// ([ADR-0028](../../../docs/adr/0028-settlement-and-payment-invariant.md)).
    BillTrigger {
        /// Settle the bill. A one-time transition.
        Settle => "settle",
        /// Void the bill before settlement.
        Void => "void",
    }
}

/// The lifecycle of a bill: `Open`, then terminal `Settled` or `Voided`.
///
/// Distinct from [`Order`]: one order may split across several bills, and several orders may merge
/// into one. `Settled` being terminal is what makes `bill:settle` a one-time transition
/// (`docs/pos-spec.md` §14.4) a property of the type rather than of a lock.
#[derive(Debug, Clone, Copy)]
pub struct Bill;

impl StateMachine for Bill {
    type State = BillState;
    type Trigger = BillTrigger;
    const NAME: &'static str = "bill";

    fn initial() -> BillState {
        BillState::Open
    }

    fn triggers() -> &'static [BillTrigger] {
        BillTrigger::ALL
    }

    fn trigger_name(trigger: BillTrigger) -> &'static str {
        trigger.label()
    }

    fn next(from: BillState, trigger: BillTrigger) -> Option<BillState> {
        Some(match (from, trigger) {
            (BillState::Open, BillTrigger::Settle) => BillState::Settled,
            (BillState::Open, BillTrigger::Void) => BillState::Voided,
            _ => return None,
        })
    }

    fn is_terminal(state: BillState) -> bool {
        matches!(state, BillState::Settled | BillState::Voided)
    }

    fn rank(state: BillState) -> u8 {
        match state {
            BillState::Open => 0,
            BillState::Settled => 1,
            BillState::Voided => 2,
            BillState::Unspecified => u8::MAX,
        }
    }
}

// -----------------------------------------------------------------------------------------------
// Shift
// -----------------------------------------------------------------------------------------------

triggers! {
    /// What moves a cash shift toward close. The two steps are separate because the close is blind:
    /// the count is recorded before the variance is revealed (`docs/pos-spec.md` §6).
    ShiftTrigger {
        /// Record the counted amount, blind.
        Count => "count",
        /// Reveal the variance and lock the shift.
        Close => "close",
    }
}

/// The lifecycle of a cash shift: `Open → Counted → Closed`.
///
/// `Counted` is its own state because folding the blind count into the close would make the
/// blindness unverifiable afterwards, which defeats the fraud control it exists for.
#[derive(Debug, Clone, Copy)]
pub struct Shift;

impl StateMachine for Shift {
    type State = ShiftState;
    type Trigger = ShiftTrigger;
    const NAME: &'static str = "shift";

    fn initial() -> ShiftState {
        ShiftState::Open
    }

    fn triggers() -> &'static [ShiftTrigger] {
        ShiftTrigger::ALL
    }

    fn trigger_name(trigger: ShiftTrigger) -> &'static str {
        trigger.label()
    }

    fn next(from: ShiftState, trigger: ShiftTrigger) -> Option<ShiftState> {
        Some(match (from, trigger) {
            (ShiftState::Open, ShiftTrigger::Count) => ShiftState::Counted,
            (ShiftState::Counted, ShiftTrigger::Close) => ShiftState::Closed,
            _ => return None,
        })
    }

    fn is_terminal(state: ShiftState) -> bool {
        matches!(state, ShiftState::Closed)
    }

    fn rank(state: ShiftState) -> u8 {
        match state {
            ShiftState::Open => 0,
            ShiftState::Counted => 1,
            ShiftState::Closed => 2,
            ShiftState::Unspecified => u8::MAX,
        }
    }
}

/// The transition tables of all five machines, rendered as one Markdown document.
///
/// `docs/state-machines.md` is generated from this, and a test keeps the committed file identical to
/// what this returns — the "documentation table is generated" half of `docs/architecture.md` §5.
#[must_use]
pub fn render_all() -> String {
    let mut out = String::from(
        "# State machines\n\n\
         Generated from `crates/pos-core/src/machines.rs`. Do not edit by hand — run\n\
         `POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-core` to regenerate. A cell is the resulting\n\
         state's wire token, or `·` when the trigger is not valid in that state.\n\n",
    );
    out.push_str(&Table::render_markdown());
    out.push('\n');
    out.push_str(&Order::render_markdown());
    out.push('\n');
    out.push_str(&OrderLine::render_markdown());
    out.push('\n');
    out.push_str(&Bill::render_markdown());
    out.push('\n');
    out.push_str(&Shift::render_markdown());
    out
}

#[cfg(test)]
mod tests {
    use super::{Bill, Order, OrderLine, Shift, Table, render_all};
    use crate::state_machine::{StateMachine, check_invariants};
    use pos_proto::OrderLineState;

    #[test]
    fn table_satisfies_every_invariant() {
        check_invariants::<Table>();
    }

    #[test]
    fn order_satisfies_every_invariant() {
        check_invariants::<Order>();
    }

    #[test]
    fn order_line_satisfies_every_invariant() {
        check_invariants::<OrderLine>();
    }

    #[test]
    fn bill_satisfies_every_invariant() {
        check_invariants::<Bill>();
    }

    #[test]
    fn shift_satisfies_every_invariant() {
        check_invariants::<Shift>();
    }

    #[test]
    fn a_void_is_never_resurrected_by_a_later_edit() {
        // The concrete failure ADR-0029 exists to prevent: device A voids a line, device B edits it
        // later. Whichever order the two observations merge in, the line stays voided.
        let voided = OrderLineState::Voided;
        let edited = OrderLineState::Fired; // a later, higher-effort non-terminal edit
        assert_eq!(OrderLine::merge(voided, edited), OrderLineState::Voided);
        assert_eq!(OrderLine::merge(edited, voided), OrderLineState::Voided);
        assert_eq!(
            OrderLine::merge(OrderLine::merge(edited, voided), OrderLineState::Added),
            OrderLineState::Voided,
            "no sequence of later edits brings a voided line back"
        );
    }

    #[test]
    fn settling_twice_is_refused_with_a_named_error() {
        // Exclusive settlement (pos-spec.md §14.4) as a property of the machine, not a lock: the
        // second settle has no transition out of the terminal Settled state.
        let settled = Bill::step(pos_proto::BillState::Open, super::BillTrigger::Settle)
            .expect("open bill settles");
        let second = Bill::step(settled, super::BillTrigger::Settle);
        let error = second.expect_err("a settled bill refuses a second settle");
        assert_eq!(error.machine, "bill");
        assert_eq!(error.from, "BILL_STATE_SETTLED");
        assert_eq!(error.trigger, "settle");
    }

    #[test]
    fn the_generated_table_matches_the_committed_doc() {
        // The generated-documentation half of architecture.md §5. If this fails after a deliberate
        // machine change, run `POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-core` to regenerate.
        let rendered = render_all();
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/state-machines.md");
        if std::env::var_os("POS_UPDATE_SNAPSHOTS").is_some() {
            std::fs::write(&path, &rendered).expect("write state-machine doc");
        }
        let committed = std::fs::read_to_string(&path)
            .expect("docs/state-machines.md exists; regenerate with POS_UPDATE_SNAPSHOTS=1");
        assert_eq!(
            rendered, committed,
            "docs/state-machines.md is stale; regenerate with POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-core"
        );
    }
}
