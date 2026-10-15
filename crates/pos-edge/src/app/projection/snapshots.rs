// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the commands read from the projection: an order's and a bill's snapshot, its sold lines,
//! the receipt and bill lines, and the bill input `pos-core` totals.

use pos_core::billing::{BillInput, BillLine, ClassBase};
use pos_core::error::DomainError;
use pos_ports::event_store::EventStore;
use pos_proto::OrderLineState;
use pos_proto::fees::FrozenFee;
use pos_proto::ids::{FeeId, OrderId, OrderLineId};
use pos_proto::money::Money;

use crate::app::{AppError, BillRecord, Edge, EdgeSession, LineRecord, OrderSnapshot, ReceiptLine};

impl<S: EventStore> Edge<S> {
    /// Everything a bill and its receipt are made of, from **one** read of the order.
    ///
    /// The channel is here for roadmap **B1.2**'s reason: it comes from the order's own
    /// `sales.order.opened`, falling back to the session's only when the order has no answer (a
    /// pre-B1.2 order, or a token this build does not understand). Two callers resolving that
    /// separately is two chances to reach for `session.sales_channel` and reintroduce the defect,
    /// which is exactly how it was written before.
    ///
    /// The **lines** are here for [ADR-0129](../../../docs/adr/0129-a-receipt-itemises-what-was-sold.md)'s:
    /// the receipt's rows have to sum to the subtotal printed under them, and they do so only if they
    /// are the very records the bases were folded from. A second read of the projection would be a
    /// second answer — a line added between the two reads, and a document that does not add up.
    ///
    /// # Errors
    ///
    /// [`AppError`] if a line's amounts cannot be summed.
    pub(in crate::app) fn order_snapshot(
        &self,
        order_id: OrderId,
        session: &EdgeSession,
    ) -> Result<OrderSnapshot, AppError> {
        self.snapshot_of(order_id, None, session)
    }

    /// The same read, narrowed to the lines **one bill** covers
    /// ([ADR-0128](../../../docs/adr/0128-a-bill-splits-and-merges.md) decision 1).
    ///
    /// This is the difference a split turns on. Before it, what a bill owed was assembled from every
    /// line of its order — so two bills on one order would each have owed all of it, and there was
    /// nothing to partition. A bill now answers for its own lines and nobody else's.
    ///
    /// # Errors
    ///
    /// [`AppError`] if a line's amounts cannot be summed.
    pub(in crate::app) fn bill_snapshot(
        &self,
        bill: &BillRecord,
        session: &EdgeSession,
    ) -> Result<OrderSnapshot, AppError> {
        self.snapshot_of(bill.order_id, Some(&bill.order_line_ids), session)
    }

    /// One read of an order, optionally narrowed to a set of its lines.
    ///
    /// `covers` of `None` — or an empty set, which is what a bill opened before ADR-0128's field
    /// existed carries — means every line of the order, which is what such a bill has always meant.
    fn snapshot_of(
        &self,
        order_id: OrderId,
        covers: Option<&[OrderLineId]>,
        session: &EdgeSession,
    ) -> Result<OrderSnapshot, AppError> {
        // The guard is taken and dropped inside the block: `class_bases` does arithmetic that can
        // fail, and holding the projection lock across a `?` is how a poisoned lock happens.
        let (lines, channel) = {
            let projection = self.lock_projection();
            let lines = match covers {
                Some(covered) if !covered.is_empty() => projection
                    .identified_lines_for_order(order_id)
                    .into_iter()
                    .filter(|(id, _)| covered.contains(id))
                    .map(|(_, record)| record)
                    .collect(),
                _ => projection.lines_for_order(order_id),
            };
            (lines, projection.order_channel(order_id))
        };
        let class_bases = Self::class_bases(&lines)?;
        let bill_lines = Self::bill_lines(&lines);
        Ok(OrderSnapshot {
            lines,
            class_bases,
            bill_lines,
            sales_channel: channel.unwrap_or(session.sales_channel),
        })
    }

    /// The lines that were actually sold: everything but the voided ones.
    ///
    /// One definition, two readers — [`Self::class_bases`] taxes them and [`Self::receipt_lines`]
    /// prints them. Written as a shared filter rather than the same `continue` in two loops, because
    /// the two disagreeing is precisely a receipt whose rows do not sum to its own subtotal
    /// (ADR-0129 decision 4).
    fn sold_lines(lines: &[LineRecord]) -> impl Iterator<Item = &LineRecord> {
        lines
            .iter()
            .filter(|line| line.state != OrderLineState::Voided)
    }

    /// The rows a receipt prints, in the order the bases were folded in (ADR-0129 decision 1).
    ///
    /// Straight off the record, which is straight off the event: the name and the unit price are the
    /// ones the guest agreed to at add time, not the ones the menu carries now (§14.2).
    pub(in crate::app) fn receipt_lines(lines: &[LineRecord]) -> Vec<ReceiptLine> {
        Self::sold_lines(lines)
            .map(|line| ReceiptLine {
                menu_item_id: line.menu_item_id,
                display_name: line.display_name.clone(),
                quantity: line.quantity,
                unit_price: line.unit_price,
                line_total: line.line_total,
                modifier_display_names: line.modifier_display_names.clone(),
                modifier_menu_item_ids: line.modifier_menu_item_ids.clone(),
            })
            .collect()
    }

    /// The sold lines as a fee rule matches them: its item, quantity, net and class
    /// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 2). The same lines
    /// [`Self::class_bases`] sums, through the same filter, so each class's lines come to its base
    /// and [`billing::assemble`] never finds them another bill's.
    pub(in crate::app) fn bill_lines(lines: &[LineRecord]) -> Vec<BillLine> {
        Self::sold_lines(lines).map(Self::bill_line).collect()
    }

    /// One sold line as a fee rule matches it. Its net is the extended total captured at add time,
    /// the figure [`Self::class_bases`] sums.
    fn bill_line(line: &LineRecord) -> BillLine {
        BillLine {
            menu_item_id: line.menu_item_id,
            quantity: line.quantity,
            net: line.line_total,
            tax_class_id: line.tax_class_id,
        }
    }

    /// Groups an order's lines into a pre-tax base per tax class, the input [`billing::assemble`]
    /// takes. A voided line is owed nothing, so it contributes nothing.
    fn class_bases(lines: &[LineRecord]) -> Result<Vec<ClassBase>, AppError> {
        let mut bases: Vec<ClassBase> = Vec::new();
        for line in Self::sold_lines(lines) {
            if let Some(base) = bases
                .iter_mut()
                .find(|base| base.tax_class_id == line.tax_class_id)
            {
                base.amount = base
                    .amount
                    .checked_add(line.line_total)
                    .map_err(DomainError::from)?;
            } else {
                bases.push(ClassBase {
                    tax_class_id: line.tax_class_id,
                    amount: line.line_total,
                });
            }
        }
        Ok(bases)
    }

    /// Builds the bill-assembly input from the session's tax configuration, one read of the lines,
    /// and the fee rules the bill is computed from. The P5 bootstrap runs with no bill-level
    /// discount, no service charge and no cash rounding; the cloud config tree (P7) supplies those,
    /// and the shape here is ready for them.
    ///
    /// The class bases and the lines the rules match come from the same [`OrderSnapshot`], so
    /// they always describe one bill: [`DomainError::LinesDoNotMatchBases`] cannot follow from a
    /// read, and answers as a server error if it ever does.
    pub(in crate::app) fn bill_input<'a>(
        session: &'a EdgeSession,
        order: &'a OrderSnapshot,
        fee_rules: &'a [FrozenFee],
        waived_fee_ids: &'a [FeeId],
        reductions: (Money, Money),
    ) -> BillInput<'a> {
        let currency = session.currency;
        let (bill_discount, comps) = reductions;
        BillInput {
            currency_code: currency,
            class_bases: &order.class_bases,
            bill_discount,
            comps,
            service_charge: Money::zero(currency),
            service_charge_taxable: true,
            service_charge_tax_class: None,
            rates: &session.tax_rates,
            // The order's channel, from the snapshot — not `session.sales_channel`, which is a
            // store-wide constant and therefore the wrong rate for every order that did not come in
            // on it.
            sales_channel: order.sales_channel,
            cash_rounding_increment: session.cash_rounding_increment,
            tax_rounding: session.tax_rounding.rounding(),
            prices_include_tax: session.prices_include_tax,
            fee_rules,
            waived_fee_ids,
            lines: &order.bill_lines,
        }
    }
}
