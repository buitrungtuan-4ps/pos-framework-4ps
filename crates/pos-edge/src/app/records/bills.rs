// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the projection remembers about bills, what a settle records of a bill's fees and taxes, and
//! the settled receipt a copy is printed from.

use pos_core::billing::{BillTotals, FeeLine, FeeWhole, MergingBill, TaxComponentLine, TaxLine};
use pos_proto::BillState;
use pos_proto::events::{BillFeeLine, BillTaxComponent, BillTaxLine, BillingBillSettled};
use pos_proto::fees::FrozenFee;
use pos_proto::ids::{FeeId, OrderId, OrderLineId, SubjectId, TableId};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::time::{BusinessDate, Timestamp};

use crate::app::session::EdgeSession;

/// A bill's fees as the settle records them and the check reads show them
/// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 4): one per fee charged,
/// in rule order, with the rule, its code and the name the bill froze, what it charged and the tax
/// on it, and that tax by component where the store's `tax` node names components
/// ([ADR-0168](../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md) decision 3).
/// One mapping for both, so the till and the log cannot describe a fee differently; the till's
/// reads name each fee in the store's display language ([`fee_records_in`]).
pub(crate) fn fee_records(totals: &BillTotals) -> Vec<BillFeeLine> {
    totals
        .fee_lines
        .iter()
        .map(|line| BillFeeLine {
            fee_id: line.fee_id,
            code: line.code.clone(),
            display_name: line.display_name.clone(),
            amount: line.amount,
            tax: line.tax,
            tax_components: component_records(&line.tax_components),
        })
        .collect()
}

/// Components as a settle records them, in the order the bill computed them: empty where the bill
/// split nothing, which leaves the field off the event.
pub(in crate::app) fn component_records(components: &[TaxComponentLine]) -> Vec<BillTaxComponent> {
    components
        .iter()
        .map(|component| BillTaxComponent {
            name: component.name.clone(),
            rate_basis_points: component.rate_basis_points,
            tax: component.tax,
        })
        .collect()
}

/// [`fee_records`] for a screen: each fee named in `language` where the store's current rule
/// translates it ([`EdgeSession::fee_translation`]), and as the bill froze it otherwise.
pub(crate) fn fee_records_in(
    totals: &BillTotals,
    session: &EdgeSession,
    language: Option<&str>,
) -> Vec<BillFeeLine> {
    let mut records = fee_records(totals);
    if let Some(language) = language {
        for record in &mut records {
            if let Some(name) = session.fee_translation(record.fee_id, language) {
                record.display_name = name.clone();
            }
        }
    }
    records
}

/// A bill's tax per class as the settle records it (ADR-0159 decision 4, roadmap-v3 B4.1): each
/// class's base, rate and tax, rounded once on that whole base and summing to the bill's tax, and
/// that tax by component where the store's `tax` node names components (ADR-0168 decision 3).
pub(in crate::app) fn tax_records(totals: &BillTotals) -> Vec<BillTaxLine> {
    totals
        .tax_lines
        .iter()
        .map(|line| BillTaxLine {
            tax_class_id: line.tax_class_id,
            taxable_base: line.taxable_base,
            rate_basis_points: line.rate_basis_points,
            tax: line.tax,
            components: component_records(&line.components),
        })
        .collect()
}

/// What the projection remembers about one bill: the order it bills, the table that order sits on
/// **if** it sits on one (so settling can cycle the table), and its state (so a second settle is
/// refused).
///
/// `table_id` is an `Option` because a bill belongs to an order, not to a table
/// ([ADR-0093](../../../docs/adr/0093-bill-keyed-on-order.md)). A takeaway or delivery order is
/// tableless by design ([ADR-0064](../../../docs/adr/0064-edge-order-in.md)); while this was a bare
/// `TableId` such an order could be accepted, priced, queued and fired — and never charged for,
/// because there was no table to open its bill against.
/// `Clone`, not `Copy`: the line set is a `Vec`. Cloning a record is a handful of words plus a list
/// the size of one table's order, and the alternative — keeping a bill's lines somewhere other than
/// beside the bill — is the shape ADR-0128 was written to replace.
#[derive(Debug, Clone)]
pub(in crate::app) struct BillRecord {
    pub(in crate::app) order_id: OrderId,
    pub(in crate::app) table_id: Option<TableId>,
    pub(in crate::app) state: BillState,
    /// The order lines this bill covers
    /// ([ADR-0128](../../../docs/adr/0128-a-bill-splits-and-merges.md) decision 1).
    ///
    /// A bill covers *lines*, not an order, which is what makes a split a partition of something.
    /// Until this field existed the amount owed was assembled by reading **every** line of the
    /// order, so there was nothing to partition and two bills on one order would each have owed all
    /// of it.
    ///
    /// **Empty means "every unvoided line on the order"**, which is what a bill opened before this
    /// field existed meant and what it still means on replay. Not a `None`, because the distinction
    /// is only ever read one way — `covered_lines` resolves it once — and an `Option<Vec<_>>` would
    /// make every reader handle two absences.
    pub(in crate::app) order_line_ids: Vec<OrderLineId>,
    /// What has been taken off this bill so far, by kind.
    ///
    /// Kept apart rather than summed, because `billing::BillInput` takes them apart — a discount is
    /// a price decision and a comp is food given away whose cost inventory still carries — and the
    /// receipt prints them on two lines. One field would have to be split again at every reader.
    ///
    /// `None` is "none yet", and is not the same as zero. A bill is opened by
    /// `billing.bill.opened`, which carries no money at all, and the replay fold has no session to
    /// ask for a currency — so a `Money::zero` here would need a currency invented at fold time,
    /// which is a worse thing to store than the absence it would be standing in for. The currency
    /// arrives with the first reduction, on the event's own `amount`.
    pub(in crate::app) discount: Option<Money>,
    pub(in crate::app) comp: Option<Money>,
    /// The fee rules in force for this bill when it opened, frozen
    /// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 3): what its
    /// `billing.bill.opened` recorded, and what it is computed from until it settles. A part of a
    /// split holds the source's, with a fee per bill cut to its share (decision 6). Empty for a
    /// bill opened before the field existed, or with no rule in force, which charges no fee.
    pub(in crate::app) fee_rules: Vec<FrozenFee>,
    /// The wholes this bill's fees per bill are shares of (ADR-0159 decision 6): its own amounts
    /// for a bill split from no other, its source's for a part, the largest of each for a merged
    /// bill. Folded from `billing.bill.opened`, `billing.bill.split` and `billing.bill.merged`,
    /// so a restart rebuilds them, and what lets a merge put a split's shares back together.
    pub(in crate::app) fee_wholes: Vec<FeeWhole>,
    /// The fees waived on this bill (ADR-0159 decision 5), in the order they were waived: folded
    /// from `billing.fee.waived`, a part of a split's taken from its source, and a merged bill's
    /// from every bill merged. The bill is computed as though their rules were not there.
    pub(in crate::app) waived_fee_ids: Vec<FeeId>,
}

impl BillRecord {
    /// This bill's side of a merge: the fee rules it holds, the wholes of its fees per bill, and
    /// the fees waived on it.
    pub(in crate::app) fn merging(&self) -> MergingBill<'_> {
        MergingBill {
            fee_rules: &self.fee_rules,
            fee_wholes: &self.fee_wholes,
            waived_fee_ids: &self.waived_fee_ids,
        }
    }

    /// Every reduction on this bill, in the store's currency — the two figures
    /// [`billing::BillInput`] takes.
    pub(in crate::app) fn reductions(&self, currency: CurrencyCode) -> (Money, Money) {
        (
            self.discount.unwrap_or_else(|| Money::zero(currency)),
            self.comp.unwrap_or_else(|| Money::zero(currency)),
        )
    }
}

/// Puts components in the order a report lists them: by name, then by rate.
pub(in crate::app) fn by_name_and_rate(components: &mut [TaxComponentLine]) {
    components.sort_by(|left, right| {
        (&left.name, left.rate_basis_points).cmp(&(&right.name, right.rate_basis_points))
    });
}

/// What a settle recorded of a bill, kept so its receipt can be printed again as a copy
/// ([ADR-0164](../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)).
///
/// The figures are the ones `billing.bill.settled` carried, because a copy never prints a figure
/// the settle did not record: the rate table can change after a bill is paid, and a copy computed
/// afresh would then disagree with the paper the guest already holds.
#[derive(Debug, Clone)]
pub(in crate::app) struct SettledReceipt {
    /// The receipt's gapless number, which every copy prints too.
    pub(in crate::app) receipt_number: u64,
    /// The sum of the lines, as settled.
    pub(in crate::app) subtotal: Money,
    /// Discounts and comps together, as the settled event records them.
    pub(in crate::app) reduction_total: Money,
    /// The service charge, as settled.
    pub(in crate::app) service_charge: Money,
    /// The tax, as settled.
    pub(in crate::app) tax_total: Money,
    /// The cash rounding, as settled.
    pub(in crate::app) rounding_adjustment: Money,
    /// What the guest paid.
    pub(in crate::app) total_due: Money,
    /// Each fee the bill was charged, as settled ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md)
    /// decision 4). Empty for a bill settled with no fee, or before the settle recorded them.
    pub(in crate::app) fee_lines: Vec<BillFeeLine>,
    /// The tax per class, as settled, each line with the named parts it recorded (ADR-0168). Empty
    /// for a bill settled before the settle recorded it, whose copy recovers the lines by computing
    /// them again.
    pub(in crate::app) tax_lines: Vec<BillTaxLine>,
    /// The corporate buyer, whose details stay in the subject store (ADR-0107).
    pub(in crate::app) buyer_subject_id: Option<SubjectId>,
    /// The business day the bill settled on: a copy is printed only on the same day.
    pub(in crate::app) business_date: BusinessDate,
    /// When it settled, for the list of today's bills.
    pub(in crate::app) settle_time: Timestamp,
    /// How many copies have been printed, from `billing.receipt.reprinted`.
    pub(in crate::app) copies: u32,
}

impl SettledReceipt {
    pub(in crate::app) fn of(
        event: &BillingBillSettled,
        business_date: BusinessDate,
        settle_time: Timestamp,
    ) -> Self {
        Self {
            receipt_number: event.receipt_number,
            subtotal: event.subtotal,
            reduction_total: event.reduction_total,
            service_charge: event.service_charge,
            tax_total: event.tax_total,
            rounding_adjustment: event.rounding_adjustment,
            total_due: event.total_due,
            fee_lines: event.fee_lines.clone(),
            tax_lines: event.tax_lines.clone(),
            buyer_subject_id: event.buyer_subject_id,
            business_date,
            settle_time,
            copies: 0,
        }
    }

    /// The totals a copy prints
    /// ([ADR-0164](../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)
    /// decision 5). Either way the paper adds up to what the guest paid.
    ///
    /// A settle that recorded its fees and its tax per class (ADR-0159 decision 4) is printed from
    /// them, whatever the rate table or the fee rules say now, each tax line with the named parts
    /// it recorded ([`Self::tax_line`]). It does not record how its reductions divide between a
    /// discount and a comp, so that is taken from `recomputed` where it adds up to the recorded
    /// figure, and otherwise printed as one line.
    ///
    /// A settle from before those were recorded prints `recomputed` when it agrees with every
    /// figure the settle recorded, which is every day the rate table has not changed since, and
    /// otherwise the recorded figures with the tax as one total.
    pub(in crate::app) fn totals(&self, recomputed: Option<&BillTotals>) -> BillTotals {
        let (discount_total, comp_total) = self.reductions(recomputed);
        if !self.tax_lines.is_empty() {
            return BillTotals {
                subtotal: self.subtotal,
                discount_total,
                comp_total,
                service_charge: self.service_charge,
                fee_lines: self.fee_lines.iter().map(Self::fee_line).collect(),
                tax_lines: self
                    .tax_lines
                    .iter()
                    .map(|line| Self::tax_line(line, recomputed))
                    .collect(),
                tax_total: self.tax_total,
                rounding_adjustment: self.rounding_adjustment,
                total_due: self.total_due,
            };
        }
        let agrees = |totals: &BillTotals| {
            totals.subtotal == self.subtotal
                && totals
                    .discount_total
                    .checked_add(totals.comp_total)
                    .is_ok_and(|reduction| reduction == self.reduction_total)
                && totals.service_charge == self.service_charge
                && totals.tax_total == self.tax_total
                && totals.rounding_adjustment == self.rounding_adjustment
                && totals.total_due == self.total_due
        };
        if let Some(totals) = recomputed.filter(|totals| agrees(totals)) {
            return totals.clone();
        }
        BillTotals {
            subtotal: self.subtotal,
            discount_total,
            comp_total,
            service_charge: self.service_charge,
            fee_lines: Vec::new(),
            tax_lines: Vec::new(),
            tax_total: self.tax_total,
            rounding_adjustment: self.rounding_adjustment,
            total_due: self.total_due,
        }
    }

    /// The recorded reductions as a discount and a comp. The event records them as one figure, so
    /// the split is taken from `recomputed` when it still adds up to that figure, and otherwise the
    /// copy prints one line for both rather than guessing how it divided.
    pub(in crate::app) fn reductions(&self, recomputed: Option<&BillTotals>) -> (Money, Money) {
        recomputed
            .filter(|totals| {
                totals
                    .discount_total
                    .checked_add(totals.comp_total)
                    .is_ok_and(|reduction| reduction == self.reduction_total)
            })
            .map_or(
                (
                    self.reduction_total,
                    Money::zero(self.total_due.currency_code),
                ),
                |totals| (totals.discount_total, totals.comp_total),
            )
    }

    /// A recorded fee as the copy prints it, under the name the bill froze.
    fn fee_line(recorded: &BillFeeLine) -> FeeLine {
        FeeLine {
            fee_id: recorded.fee_id,
            code: recorded.code.clone(),
            display_name: recorded.display_name.clone(),
            amount: recorded.amount,
            class_shares: Vec::new(),
            tax: recorded.tax,
            tax_components: recorded
                .tax_components
                .iter()
                .map(Self::component_line)
                .collect(),
            // A settled bill owes nothing more to waive.
            waivable: false,
        }
    }

    /// A recorded tax line as the copy prints it, with its named parts (ADR-0104).
    ///
    /// The parts the settle recorded, whatever the rate table says now (ADR-0168 decision 4). A
    /// line that recorded none, as every line settled before the settle recorded them, takes them
    /// as every copy did before: from `recomputed`'s line of the same class, rate and tax, so they
    /// sum to it, and otherwise prints without them.
    fn tax_line(recorded: &BillTaxLine, recomputed: Option<&BillTotals>) -> TaxLine {
        let components = if recorded.components.is_empty() {
            recomputed
                .and_then(|totals| {
                    totals.tax_lines.iter().find(|line| {
                        line.tax_class_id == recorded.tax_class_id
                            && line.rate_basis_points == recorded.rate_basis_points
                            && line.tax == recorded.tax
                    })
                })
                .map(|line| line.components.clone())
                .unwrap_or_default()
        } else {
            recorded
                .components
                .iter()
                .map(Self::component_line)
                .collect()
        };
        TaxLine {
            tax_class_id: recorded.tax_class_id,
            taxable_base: recorded.taxable_base,
            rate_basis_points: recorded.rate_basis_points,
            tax: recorded.tax,
            components,
        }
    }

    /// A recorded component as the copy prints it.
    fn component_line(recorded: &BillTaxComponent) -> TaxComponentLine {
        TaxComponentLine {
            name: recorded.name.clone(),
            rate_basis_points: recorded.rate_basis_points,
            tax: recorded.tax,
        }
    }
}
