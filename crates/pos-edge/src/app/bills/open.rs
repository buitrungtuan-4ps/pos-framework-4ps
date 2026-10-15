// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Opening the bill for a table's order or for an order by id.

use pos_core::billing;
use pos_core::decision::{Actor, TableCommand, decide_table};
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::BillState;
use pos_proto::events::BillingBillOpened;
use pos_proto::ids::{BillId, OrderId, TableId};

use crate::app::{AppError, BillRecord, BillView, Edge};

impl<S: EventStore> Edge<S> {
    /// Opens a bill on the order a table holds (`billing.bill.opened`) and moves the table to
    /// awaiting payment.
    ///
    /// Splitting, merging and settling all presuppose a bill that something created; opening it is
    /// the "request bill" moment of the floor cycle, so the table moves `Occupied → AwaitingPayment`
    /// here. The table's fine-grained state is derived from the bill lifecycle rather than from its
    /// own events, because the frozen catalogue has no table-transition event for it.
    ///
    /// This is the floor's entry point and it delegates to [`Self::open_bill_for_order`], which is
    /// the primitive (ADR-0093). The delegation keeps the `Occupied` gate ADR-0072 owns inside the
    /// domain rather than moving it into whichever caller happens to know about tables.
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] if the table is not occupied (requesting the bill is illegal otherwise),
    /// [`AppError::NoOpenOrder`] if the table has no order, [`AppError::BillAlreadyOpen`] if its
    /// order already has a bill, or [`AppError`] if the store cannot be written.
    pub async fn open_bill(&self, actor: Actor, table_id: TableId) -> Result<BillView, AppError> {
        let open_order = self.lock_projection().order_for_table(table_id);
        if let Some(order_id) = open_order {
            return self.open_bill_for_order(actor, order_id).await;
        }
        // Which refusal "no order here" is belongs to the table machine, not to this function:
        // asking a *free* table for its bill is an illegal floor move and reads as one, and only a
        // table that is occupied yet somehow orderless is `NoOpenOrder`. So `decide_table` runs
        // here for its refusal rather than its result — it errors on a free table and succeeds on
        // an occupied one — which keeps the answer the floor has always given.
        let ctx = self.decision_ctx(actor)?;
        let _requestable =
            decide_table(self.table_state(table_id), TableCommand::RequestBill, &ctx)?;
        Err(AppError::NoOpenOrder)
    }

    /// Opens a bill on an **order** (`billing.bill.opened`), moving its table to awaiting payment if
    /// it has one. The primitive [`Self::open_bill`] delegates to.
    ///
    /// Keyed on the order because that is what a bill bills
    /// ([ADR-0093](../../../docs/adr/0093-bill-keyed-on-order.md)). `BillingBillOpened` has always
    /// carried `{bill_id, order_id}` and no billing event mentions a table at all, so the durable
    /// truth already said this; what was table-shaped was the edge's in-memory reach for the order,
    /// and it made takeaway revenue uncollectable — a tableless order (ADR-0064) had no table to
    /// pass, to be occupied, or to resolve from.
    ///
    /// A floor order still makes the floor move: its table is stepped `Occupied → AwaitingPayment`
    /// by `decide_table`, which is also where [`Capability::Tables`](pos_core::capability::Capability)
    /// is required. A counter order makes no floor move, so it needs neither — the capability gates
    /// *table service*, not the act of charging a guest, and a takeaway-only store must be able to
    /// take money without turning table service on.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownOrder`] if the edge knows no such order; [`AppError::BillAlreadyOpen`] if
    /// one is already open on it; [`AppError::Domain`] if the actor lacks `billing.bill.open` or the
    /// order sits on a table that is not occupied; or [`AppError`] if the store cannot be written.
    pub async fn open_bill_for_order(
        &self,
        actor: Actor,
        order_id: OrderId,
    ) -> Result<BillView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        ctx.require(Permission::OpenBill)?;

        let (existing_bill, table_id, has_lines) = {
            let projection = self.lock_projection();
            (
                projection.bill_for_order(order_id),
                projection.table_for_order(order_id),
                !projection.lines_for_order(order_id).is_empty(),
            )
        };
        if existing_bill.is_some() {
            return Err(AppError::BillAlreadyOpen);
        }
        // An order the edge does not know cannot be billed. A floor order is proved by the table
        // index; a counter order has no index, so its lines are the proof — and an inbound order
        // always opens with at least one, because `submit` refuses an empty one (ADR-0064). Checked
        // this way rather than on lines alone so a seated table whose every line was voided still
        // opens a bill and is refused later by `assemble`, which is what the floor already did.
        if table_id.is_none() && !has_lines {
            return Err(AppError::UnknownOrder);
        }
        // Nothing sold, nothing to bill (ADR-0163). A bill on an order whose every line was voided
        // could never settle, `assemble` refusing an empty set of tax bases, and its table would wait
        // for a payment that cannot come; the way out for such a table is to release it.
        if self.lock_projection().sold_line_ids(order_id).is_empty() {
            return Err(AppError::NothingToBill);
        }

        // Requesting the bill is a legal floor move only from Occupied; `decide_table` also gates
        // the tables capability. There is no floor move to prove for a counter order.
        let table_decision = match table_id {
            Some(table_id) => Some(decide_table(
                self.table_state(table_id),
                TableCommand::RequestBill,
                &ctx,
            )?),
            None => None,
        };

        // Every unvoided line on the order, named rather than implied (ADR-0128 decision 9). This
        // is exactly what opening a bill has always meant; writing it down is what lets a split
        // partition it, and what lets the log say which bill owed what without a live projection.
        let (order_line_ids, channel) = {
            let projection = self.lock_projection();
            (
                projection.sold_line_ids(order_id),
                projection.order_channel(order_id),
            )
        };
        // The fee rules in force for this bill, frozen onto it (ADR-0159 decision 3): those for the
        // channel it is taxed at, which is the order's, so a publish during the meal does not
        // change what it owes. Opening one again after a void freezes what is in force then.
        let session = self.session();
        let fee_rules = session
            .fees
            .in_force(channel.unwrap_or(session.sales_channel));

        let bill_id = BillId::new(self.next_ulid());
        let payload = BillingBillOpened {
            bill_id,
            order_id,
            order_line_ids: order_line_ids.clone(),
            fee_rules: fee_rules.clone(),
        };
        self.commit_and_publish(&ctx, &payload).await?;

        {
            let mut projection = self.lock_projection();
            projection.open_bill_record(
                bill_id,
                BillRecord {
                    order_id,
                    table_id,
                    state: BillState::Open,
                    order_line_ids,
                    discount: None,
                    comp: None,
                    fee_wholes: billing::fee_wholes(&fee_rules),
                    fee_rules,
                    waived_fee_ids: Vec::new(),
                },
            );
            if let (Some(table_id), Some(decision)) = (table_id, table_decision.as_ref()) {
                projection.set_table(table_id, decision.next_state);
            }
        }
        Ok(BillView {
            bill_id,
            state: BillState::Open,
            receipt_number: None,
            total_due: None,
            totals: None,
            // Nothing prints on an open bill, so there is nothing to carry out of it.
            lines: Vec::new(),
            table_state: table_decision.map(|decision| decision.next_state),
            print_receipt: false,
            open_drawer: false,
            drawer_till: None,
        })
    }
}
