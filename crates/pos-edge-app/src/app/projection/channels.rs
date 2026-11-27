// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A shift's settled bills by channel and order type, and each delivery app's in-app payments
//! ([ADR-0259](../../../../../docs/adr/0259-a-sale-is-reported-by-its-channel-and-its-order-type.md)
//! decisions 4 and 5).
//!
//! A bill's channel is read off its order's opening by `pos_proto::sale_channel::sale_channel`, the
//! function the cloud's rollup calls too, so the till's X and the console's X/Z put each bill on the
//! same line. A bill whose order this edge never saw open is
//! [`SaleChannel::NOT_RECORDED`]'s, so the lines always sum to the shift's bills and sales. Folded
//! the same way live and on replay, beside the settle and the tender each comes with; a void after
//! payment takes both back out while the shift is open, as it takes the sale (ADR-0258).

use pos_core::error::DomainError;
use pos_proto::PaymentMethod;
use pos_proto::ids::{BillId, ShiftId, VendorId};
use pos_proto::money::Money;
use pos_proto::sale_channel::{SaleChannel, sale_channel};

use crate::app::{Projection, ShiftRecord};

impl Projection {
    /// Where `bill_id`'s order came from and how it is served.
    pub(in crate::app) fn sale_channel_of(&self, bill_id: BillId) -> SaleChannel {
        let Some(order_id) = self.bills.get(&bill_id).map(|bill| bill.order_id) else {
            return SaleChannel::NOT_RECORDED;
        };
        self.order_origin(order_id)
            .map_or(SaleChannel::NOT_RECORDED, |origin| {
                sale_channel(origin.channel, origin.table_id, self.vendor(order_id))
            })
    }

    /// Adds a settled bill of `total` to its line on `shift`.
    pub(in crate::app) fn record_shift_channel(
        &mut self,
        shift: Option<ShiftId>,
        bill_id: BillId,
        total: Money,
    ) -> Result<(), DomainError> {
        let channel = self.sale_channel_of(bill_id);
        if let Some(record) = shift.and_then(|shift| self.shifts.get_mut(&shift)) {
            add_line(record, channel, 1, total)?;
            record.bill_channels.insert(bill_id, channel);
        }
        Ok(())
    }

    /// Adds an in-app payment towards `bill_id` to its app on `shift`; any other tender, or an
    /// in-app payment on an order no app sent, is no app's.
    pub(in crate::app) fn record_shift_in_app(
        &mut self,
        (shift, bill_id): (Option<ShiftId>, BillId),
        method: PaymentMethod,
        applied: Money,
    ) -> Result<(), DomainError> {
        if method != PaymentMethod::Vendor {
            return Ok(());
        }
        let Some(vendor_id) = self.sale_channel_of(bill_id).vendor_id else {
            return Ok(());
        };
        if let Some(record) = shift.and_then(|shift| self.shifts.get_mut(&shift)) {
            add_in_app(record, vendor_id, applied, false)?;
            record
                .bill_in_app
                .entry(bill_id)
                .or_default()
                .push((vendor_id, applied));
        }
        Ok(())
    }
}

/// Takes `bill_id`'s in-app payments out of `record`, and its line too where `total` says the bill
/// settled on it.
pub(in crate::app) fn take_channel_out(
    record: &mut ShiftRecord,
    bill_id: BillId,
    total: Option<Money>,
) -> Result<(), DomainError> {
    for (vendor_id, applied) in record.bill_in_app.remove(&bill_id).unwrap_or_default() {
        add_in_app(record, vendor_id, applied, true)?;
    }
    if let (Some(total), Some(channel)) = (total, record.bill_channels.remove(&bill_id)) {
        let zero = Money::zero(total.currency_code);
        add_line(record, channel, -1, zero.checked_sub(total)?)?;
    }
    Ok(())
}

/// Adds `bills` and `sales` to `channel`'s line, kept in order; a line left with nothing goes.
fn add_line(
    record: &mut ShiftRecord,
    channel: SaleChannel,
    bills: i32,
    sales: Money,
) -> Result<(), DomainError> {
    let at = match record
        .channels
        .binary_search_by_key(&channel, |(kept, _, _)| *kept)
    {
        Ok(at) => at,
        Err(at) => {
            record
                .channels
                .insert(at, (channel, 0, Money::zero(sales.currency_code)));
            at
        }
    };
    if let Some(line) = record.channels.get_mut(at) {
        line.1 = line.1.saturating_add_signed(bills);
        line.2 = line.2.checked_add(sales)?;
        if line.1 == 0 && line.2.is_zero() {
            record.channels.remove(at);
        }
    }
    Ok(())
}

/// Adds (or, `back`, takes) `applied` to `vendor_id`'s in-app total, kept in order of connection.
fn add_in_app(
    record: &mut ShiftRecord,
    vendor_id: VendorId,
    applied: Money,
    back: bool,
) -> Result<(), DomainError> {
    let at = match record
        .in_app
        .binary_search_by_key(&vendor_id, |(kept, _)| *kept)
    {
        Ok(at) => at,
        Err(at) => {
            record
                .in_app
                .insert(at, (vendor_id, Money::zero(applied.currency_code)));
            at
        }
    };
    if let Some(total) = record.in_app.get_mut(at) {
        total.1 = if back {
            total.1.checked_sub(applied)?
        } else {
            total.1.checked_add(applied)?
        };
    }
    Ok(())
}
