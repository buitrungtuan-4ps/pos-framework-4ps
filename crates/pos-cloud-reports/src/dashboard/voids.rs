// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A bill voided after payment, taken back out of its day
//! ([ADR-0184](../../../../docs/adr/0184-a-settled-bill-is-voided-whole-not-refunded.md) decision 9,
//! and its second amendment).
//!
//! `billing.bill.voided` carries the bill and the reason and nothing else, so the rollup keeps, for
//! each trading day, the bills settled on it: what each took, its tax, fees and tax components, and
//! its cash. A void of one of them takes exactly those figures back out of the bill's own day, which
//! then counts it under "voided after payment", and adds its cash to what the drawer of the void's
//! shift handed back. The fold stays one event at a time
//! ([ADR-0036](../../../../docs/adr/0036-materialised-rollups.md)), so the materialised rollup and a
//! fold of the whole log agree; the cost is a rollup that grows with each day's bills.
//!
//! Store-level only: nothing here is kept or reported per person.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use pos_proto::PaymentMethod;
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::{
    BillingBillSettled, BillingBillVoided, BillingPaymentCaptured, CashShiftClosed, CashShiftOpened,
};

use super::rollup::{empty_cash, empty_revenue};
use crate::types::{DailyCash, DailyRevenue, STORE_DRAWER};

/// What a settled bill added to its day, kept so a void can take it back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptBill {
    /// `total_due`, minor units.
    pub total_due: i64,
    /// `subtotal`.
    pub subtotal: i64,
    /// `reduction_total`.
    pub reductions: i64,
    /// `service_charge`.
    pub service_charge: i64,
    /// `tax_total`.
    pub tax: i64,
    /// Each fee line it charged: code, amount, tax.
    pub fees: Vec<(String, i64, i64)>,
    /// Each tax component its tax lines recorded: name, rate in basis points, tax.
    pub components: Vec<(String, u32, i64)>,
    /// The cash its payments applied to it.
    pub cash: i64,
}

/// What the rollup keeps to fold voids after payment: each day's settled bills, the cash a bill's
/// payments applied until its settle lands, and the drawer of each open shift.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptBills {
    /// Each trading day's settled bills, keyed by day and then by bill id.
    #[serde(default)]
    pub days: BTreeMap<String, BTreeMap<String, KeptBill>>,
    /// Cash applied to a bill whose settle has not been folded yet. The payments and the settle are
    /// one transaction, so an entry lives until the next event.
    #[serde(default)]
    pub pending_cash: BTreeMap<String, i64>,
    /// The drawer each open shift counts, its till's id or [`STORE_DRAWER`], from
    /// `cash.shift.opened` until `cash.shift.closed`.
    #[serde(default)]
    pub shift_drawers: BTreeMap<String, String>,
}

/// Folds one event into what is kept for voids, and a void after payment into `revenue` and `cash`.
/// Every other event is ignored, and a payload that fails to decode is skipped, as the other folds
/// skip one.
pub fn fold_voids(
    kept: &mut KeptBills,
    revenue: &mut BTreeMap<String, DailyRevenue>,
    cash: &mut BTreeMap<String, DailyCash>,
    event: &EventEnvelope<RawPayload>,
) {
    match event.event_type.as_str() {
        "billing.payment.captured" => {
            if let Ok(payment) = event.data.decode::<BillingPaymentCaptured>()
                && payment.method.known() == PaymentMethod::Cash
            {
                let slot = kept
                    .pending_cash
                    .entry(payment.bill_id.to_string())
                    .or_insert(0);
                *slot = slot.saturating_add(payment.applied_to_bill.amount_minor);
            }
        }
        "billing.bill.settled" => {
            if let Ok(bill) = event.data.decode::<BillingBillSettled>() {
                let id = bill.bill_id.to_string();
                let cash = kept.pending_cash.remove(&id).unwrap_or(0);
                kept.days
                    .entry(event.business_date.to_string())
                    .or_default()
                    .insert(id, kept_bill(&bill, cash));
            }
        }
        "cash.shift.opened" => {
            if let Ok(opened) = event.data.decode::<CashShiftOpened>() {
                let drawer = opened
                    .terminal_device_id
                    .map_or_else(|| STORE_DRAWER.to_owned(), |till| till.to_string());
                kept.shift_drawers
                    .insert(opened.opened_shift_id.to_string(), drawer);
            }
        }
        "cash.shift.closed" => {
            if let Ok(closed) = event.data.decode::<CashShiftClosed>() {
                kept.shift_drawers
                    .remove(&closed.closed_shift_id.to_string());
            }
        }
        "billing.bill.voided" => {
            if let Ok(voided) = event.data.decode::<BillingBillVoided>() {
                fold_void(kept, revenue, cash, event, &voided.bill_id.to_string());
            }
        }
        _ => {}
    }
}

/// What a settle added to its day, with the cash its payments applied.
fn kept_bill(bill: &BillingBillSettled, cash: i64) -> KeptBill {
    KeptBill {
        total_due: bill.total_due.amount_minor,
        subtotal: bill.subtotal.amount_minor,
        reductions: bill.reduction_total.amount_minor,
        service_charge: bill.service_charge.amount_minor,
        tax: bill.tax_total.amount_minor,
        fees: fees_by_code(bill),
        components: bill
            .tax_lines
            .iter()
            .flat_map(|line| &line.components)
            .map(|component| {
                (
                    component.name.as_str().to_owned(),
                    component.rate_basis_points,
                    component.tax.amount_minor,
                )
            })
            .collect(),
        cash,
    }
}

/// Its fee lines by code, as the revenue fold counts them: a line that charged and taxed nothing is
/// left out, and two lines under one code are one bill charging it.
fn fees_by_code(bill: &BillingBillSettled) -> Vec<(String, i64, i64)> {
    let mut fees: Vec<(String, i64, i64)> = Vec::new();
    for line in &bill.fee_lines {
        if line.amount.is_zero() && line.tax.is_zero() {
            continue;
        }
        let code = line.code.as_str();
        match fees.iter_mut().find(|(kept, _, _)| kept == code) {
            Some(fee) => {
                fee.1 = fee.1.saturating_add(line.amount.amount_minor);
                fee.2 = fee.2.saturating_add(line.tax.amount_minor);
            }
            None => fees.push((
                code.to_owned(),
                line.amount.amount_minor,
                line.tax.amount_minor,
            )),
        }
    }
    fees
}

/// Takes a voided bill's figures back out of the day it settled on, counts it there as voided after
/// payment, and adds its cash to what the void's drawer handed back on the void's day. A void of a
/// bill the rollup kept no settle for — one voided before it was paid — changes nothing here.
fn fold_void(
    kept: &mut KeptBills,
    revenue: &mut BTreeMap<String, DailyRevenue>,
    cash: &mut BTreeMap<String, DailyCash>,
    event: &EventEnvelope<RawPayload>,
    bill_id: &str,
) {
    // Its own day first, which is where every void inside the business-date window finds it.
    let today = event.business_date.to_string();
    let found = kept
        .days
        .get_mut(&today)
        .and_then(|bills| bills.remove(bill_id))
        .map(|bill| (today.clone(), bill))
        .or_else(|| {
            kept.days
                .iter_mut()
                .rev()
                .find_map(|(day, bills)| bills.remove(bill_id).map(|bill| (day.clone(), bill)))
        });
    let Some((settled_on, bill)) = found else {
        return;
    };
    let day = revenue
        .entry(settled_on.clone())
        .or_insert_with(|| empty_revenue(&settled_on));
    take_back(day, &bill);

    if bill.cash != 0 {
        let drawer = event
            .shift_id
            .and_then(|shift| kept.shift_drawers.get(&shift.to_string()).cloned())
            .unwrap_or_else(|| STORE_DRAWER.to_owned());
        let day = cash
            .entry(today.clone())
            .or_insert_with(|| empty_cash(&today));
        day.voided_cash = day.voided_cash.saturating_add(bill.cash);
        let figures = day.by_drawer.entry(drawer).or_default();
        figures.voided_cash = figures.voided_cash.saturating_add(bill.cash);
    }
}

/// Subtracts what `bill`'s settle added to `day`, and counts it as voided after payment.
fn take_back(day: &mut DailyRevenue, bill: &KeptBill) {
    day.bills = day.bills.saturating_sub(1);
    day.gross = day.gross.saturating_sub(bill.subtotal);
    day.reductions = day.reductions.saturating_sub(bill.reductions);
    day.service_charge = day.service_charge.saturating_sub(bill.service_charge);
    day.tax = day.tax.saturating_sub(bill.tax);
    day.net = day.net.saturating_sub(bill.total_due);
    for (code, amount, tax) in &bill.fees {
        let emptied = day.by_fee.get_mut(code).is_some_and(|total| {
            total.amount = total.amount.saturating_sub(*amount);
            total.tax = total.tax.saturating_sub(*tax);
            total.bills = total.bills.saturating_sub(1);
            total.bills == 0
        });
        if emptied {
            day.by_fee.remove(code);
        }
    }
    for (name, rate, tax) in &bill.components {
        if let Some(at) = day
            .by_tax_component
            .iter()
            .position(|total| total.component_name == *name && total.rate_basis_points == *rate)
            && let Some(total) = day.by_tax_component.get_mut(at)
        {
            total.tax = total.tax.saturating_sub(*tax);
        }
    }
    day.voided_after_payment.bills = day.voided_after_payment.bills.saturating_add(1);
    day.voided_after_payment.amount = day
        .voided_after_payment
        .amount
        .saturating_add(bill.total_due);
}

#[cfg(test)]
mod tests;
