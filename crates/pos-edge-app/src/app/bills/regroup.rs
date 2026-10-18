// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Splitting a bill into parts and merging bills into one, and the lines each covers.

use pos_core::billing;
use pos_core::billing::BillLine;
use pos_core::decision::{Actor, BillCommand, decide_bill};
use pos_ports::event_store::EventStore;
use pos_proto::BillState;
use pos_proto::events::{BillingBillMerged, BillingBillOpened, BillingBillSplit};
use pos_proto::ids::{BillId, OrderId, OrderLineId};

use crate::app::{AppError, BillRecord, Edge, LineRecord};

impl<S: EventStore> Edge<S> {
    /// Partitions a bill's lines into two or more new bills
    /// ([ADR-0128](../../../../../docs/adr/0128-a-bill-splits-and-merges.md)).
    ///
    /// The source moves to `SPLIT`, which is terminal: it stops owing anything and the parts owe it
    /// instead, so the same food cannot be charged twice. Each part becomes an ordinary bill on the
    /// same order and the same table, and can itself be split again — nothing tracks a depth.
    ///
    /// **One transaction.** Every part's `billing.bill.opened` and the `billing.bill.split` naming
    /// them are appended together or not at all, so the log never holds parts whose source is
    /// missing, or a source whose parts are (decision 10).
    ///
    /// **Each part computes its own tax and its own rounding**, because `assemble` runs per bill over
    /// that bill's captured line totals. The parts' totals can therefore differ from the source's by
    /// the cash rounding on each, and that is the correct answer rather than a discrepancy: the
    /// source's total was quoted and never charged, and what the store banks is the sum of what each
    /// guest was actually asked for (decision 4).
    ///
    /// `billing.bill.split` and no PIN: nothing is created, forgiven or moved out of the store, so
    /// no manager is asked, and the permission lets a role say who splits (ADR-0158).
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownBill`] if no such bill; [`AppError::Domain`] if the bill is not open, the
    /// actor lacks `billing.bill.split`, or the proposed parts are not a partition of what it
    /// covers.
    pub async fn split_bill(
        &self,
        actor: Actor,
        bill_id: BillId,
        parts: Vec<Vec<OrderLineId>>,
    ) -> Result<Vec<BillId>, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let bill = self
            .lock_projection()
            .bill(bill_id)
            .ok_or(AppError::UnknownBill)?;

        // What the bill covers, resolved the one way the projection resolves it — so a bill opened
        // before the line set was recorded splits on the same lines it would have been settled on.
        let covers = self.covered_lines(&bill);
        let decided = decide_bill(
            bill.state,
            BillCommand::Split {
                covers,
                parts: parts.clone(),
            },
            &ctx,
        )?;

        // One id per part, minted before anything is written so the split event can name them.
        let part_ids: Vec<BillId> = parts
            .iter()
            .map(|_| BillId::new(self.next_ulid()))
            .collect();

        // Each part keeps the rules the bill froze when it opened, not what is in force now, so a
        // publish during the meal reaches no part of it (ADR-0159 decision 3). A fee per bill is
        // shared across the parts in proportion to the lines it counts on each, rather than
        // charged again on every part (decision 6).
        let part_rules =
            billing::split_fee_rules(&bill.fee_rules, &self.part_lines(bill.order_id, &parts))?;

        let mut envelopes = Vec::new();
        let mut messages = Vec::new();
        for ((part_id, lines), fee_rules) in part_ids.iter().zip(parts.iter()).zip(&part_rules) {
            let opened = BillingBillOpened {
                bill_id: *part_id,
                order_id: bill.order_id,
                order_line_ids: lines.clone(),
                fee_rules: fee_rules.clone(),
            };
            let (envelope, message) = self.prepare(&ctx, &opened)?;
            envelopes.push(envelope);
            messages.push(message);
        }
        let split = BillingBillSplit {
            source_bill_id: bill_id,
            resulting_bill_ids: part_ids.clone(),
        };
        let (envelope, message) = self.prepare(&ctx, &split)?;
        envelopes.push(envelope);
        messages.push(message);
        self.append_and_publish(envelopes, messages).await?;

        {
            let mut projection = self.lock_projection();
            for ((part_id, lines), fee_rules) in
                part_ids.iter().zip(parts.into_iter()).zip(part_rules)
            {
                projection.open_bill_record(
                    *part_id,
                    BillRecord {
                        order_id: bill.order_id,
                        table_id: bill.table_id,
                        state: BillState::Open,
                        order_line_ids: lines,
                        // A part starts owing its lines in full. The source's reductions are not
                        // divided across the parts: a discount was a decision about *that* bill, and
                        // splitting one is not the arithmetic that says how it should land on two.
                        // Reducing a part is an ordinary discount on an ordinary bill.
                        discount: None,
                        comp: None,
                        fee_wholes: billing::fee_wholes(&fee_rules),
                        fee_rules,
                        waived_fee_ids: Vec::new(),
                    },
                );
            }
            // As a replay folds the split after the parts' openings (ADR-0159 decisions 5 and 6).
            projection.inherit_fees(bill_id, &part_ids);
            projection.set_bill_state(bill_id, decided.next_state);
        }
        Ok(part_ids)
    }

    /// Folds `absorbed` into `bill_id`, which takes their lines and survives
    /// ([ADR-0128](../../../../../docs/adr/0128-a-bill-splits-and-merges.md) decision 5).
    ///
    /// The target keeps its identity because it is the bill the cashier is standing in front of, so
    /// a merge is not a split in reverse and does not need to be. Every absorbed bill must be open,
    /// and each moves to `MERGED`, which is terminal.
    ///
    /// **Restricted to bills of the target's order**, which the model does not require. The floor
    /// requires the same table: settling a bill moves its table from `AwaitingPayment` to
    /// `NeedsCleaning`, and a bill spanning two tables makes *"which table moved?"* a question with
    /// two answers. The money requires the same order: a settle reads a bill's lines from its own
    /// order, so another order's lines would be charged nothing, and the bill that held them, merged,
    /// could never be paid. So two tableless counter bills merge only within one order, as the parts
    /// of a split do. Both are checked before anything is written.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownBill`] if the target or any absorbed bill is unknown;
    /// [`AppError::BillsOnDifferentTables`] if they do not share a table, or the target is among the
    /// absorbed; [`AppError::BillsOnDifferentOrders`] if they share a table but not an order;
    /// [`AppError::Domain`] if the target or an absorbed bill is not open, or the actor lacks
    /// `billing.bill.split`.
    pub async fn merge_bills(
        &self,
        actor: Actor,
        bill_id: BillId,
        absorbed: Vec<BillId>,
    ) -> Result<BillId, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let target = self
            .lock_projection()
            .bill(bill_id)
            .ok_or(AppError::UnknownBill)?;
        // The target has to be open too, and the machine is what says so — a settled bill gaining
        // lines would be food charged after the money was taken. Asked through the same door the
        // absorbed bills go through, so the two cannot drift apart, and the answer is **discarded**:
        // the target survives a merge rather than moving, so there is no state to write.
        let _open = decide_bill(target.state, BillCommand::Merge, &ctx)?;

        let mut folded = Vec::new();
        for absorbed_id in &absorbed {
            // Merging a bill into itself would double its own lines, which is the one arithmetic
            // mistake this whole record exists to make impossible.
            if *absorbed_id == bill_id {
                return Err(AppError::BillsOnDifferentTables);
            }
            let record = self
                .lock_projection()
                .bill(*absorbed_id)
                .ok_or(AppError::UnknownBill)?;
            if record.table_id != target.table_id {
                return Err(AppError::BillsOnDifferentTables);
            }
            // Two counter bills share a table — none — and are still two orders. Merged, the
            // absorbed order's lines would ride on a bill that charges only the target's.
            if record.order_id != target.order_id {
                return Err(AppError::BillsOnDifferentOrders);
            }
            let decided = decide_bill(record.state, BillCommand::Merge, &ctx)?;
            folded.push((*absorbed_id, record, decided.next_state));
        }

        // What the merged bill holds (ADR-0159 decision 6): the target's rules, and each fee per
        // bill at the sum of the bills' shares of it, capped at its whole. Decided before anything
        // is written, so a merge whose shares overflow is refused rather than half-recorded.
        let merged_fees = self.lock_projection().merged_fees(bill_id, &absorbed)?;

        let event = BillingBillMerged {
            target_bill_id: bill_id,
            merged_bill_ids: absorbed,
        };
        self.commit_and_publish(&ctx, &event).await?;

        {
            // Resolved through the guard already held. `Edge::covered_lines` takes the lock itself,
            // and calling it here re-locked a `std::sync::Mutex` on the thread holding it: for a
            // bill that names no lines — every bill opened before ADR-0128 recorded them — that is
            // a deadlock, and with the projection locked every till on the store hangs with it.
            let mut projection = self.lock_projection();
            let mut lines = projection.covered_lines(&target);
            for (absorbed_id, record, next_state) in folded {
                lines.extend(projection.covered_lines(&record));
                projection.set_bill_state(absorbed_id, next_state);
            }
            lines.sort_unstable();
            lines.dedup();
            projection.set_bill_lines(bill_id, lines);
            if let Some(fees) = merged_fees {
                projection.set_bill_fees(bill_id, fees);
            }
        }
        Ok(bill_id)
    }

    /// The lines a bill covers, resolving the "empty means all of them" case once.
    ///
    /// A bill opened before ADR-0128's field existed names none, and meant every unvoided line on
    /// its order — so that is what it still covers. Resolved here rather than at each reader,
    /// because two readers disagreeing about what a bill covers is two different answers to what a
    /// guest owes.
    pub(in crate::app) fn covered_lines(&self, bill: &BillRecord) -> Vec<OrderLineId> {
        self.lock_projection().covered_lines(bill)
    }

    /// Each proposed part's sold lines as a fee rule matches them, read through
    /// [`Self::bill_lines`] as a bill's are, so a part's share of a fee per bill is weighed on the
    /// lines it will owe.
    fn part_lines(&self, order_id: OrderId, parts: &[Vec<OrderLineId>]) -> Vec<Vec<BillLine>> {
        let lines = self.lock_projection().identified_lines_for_order(order_id);
        parts
            .iter()
            .map(|part| {
                let covered: Vec<LineRecord> = lines
                    .iter()
                    .filter(|(id, _)| part.contains(id))
                    .map(|(_, line)| line.clone())
                    .collect();
                Self::bill_lines(&covered)
            })
            .collect()
    }
}
