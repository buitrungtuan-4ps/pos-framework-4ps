// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Overriding a line's price before its bill opens, with a reason and an approver
//! ([ADR-0219](../../../../../docs/adr/0219-a-comp-and-a-price-override-each-need-a-reason-and-an-approver.md)
//! decision 2).

use pos_core::decision::Actor;
use pos_core::decision::reprice::{Reprice, decide_reprice, reprice_bounds};
use pos_core::error::DomainError;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::OrderLineState;
use pos_proto::events::SalesOrderLineRepriced;
use pos_proto::ids::{OrderLineId, ReasonCodeId};
use pos_proto::money::{Money, Rounding};
use pos_proto::reason_codes::ReasonAction;

use crate::app::{AppError, Approval, Edge};

impl<S: EventStore> Edge<S> {
    /// Overrides one line's price of one, before its bill opens (ADR-0219 decision 2), and answers
    /// with what it recorded.
    ///
    /// The new total is the new price times the line's quantity, computed here, as a quantity
    /// change computes it. The price must be above zero and at most the store's
    /// `billing.price_override_max_multiple` of the price the line was **added** at, never of a price
    /// an earlier override set. [`Permission::OverridePrice`] is PIN-flagged, so the step-up runs as
    /// a comp's does and the approval is recorded beside `sales.order_line.repriced` in one
    /// transaction. Tax and promotions follow the new price as from any price.
    ///
    /// Everything that could never be overridden is refused before anybody is asked to approve it:
    /// a voided line, a line whose order has a bill, a reason the store does not publish for an
    /// override, and a price outside the bounds.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownLine`] for a line the edge does not hold or one voided;
    /// [`AppError::BillAlreadyOpen`] once a bill is open on the order; [`AppError::ReasonCodeNotValid`];
    /// [`AppError::Domain`] with [`DomainError::PriceOutOfRange`] or a missing permission;
    /// [`AppError::ApprovalRequired`], [`AppError::ApprovalRefused`] or
    /// [`AppError::ApproverLockedOut`] from the step-up; [`AppError::Port`] if the store cannot be
    /// written.
    pub async fn reprice_line(
        &self,
        actor: Actor,
        order_line_id: OrderLineId,
        unit_price: Money,
        reason_code_id: ReasonCodeId,
        approval: Option<&Approval>,
    ) -> Result<SalesOrderLineRepriced, AppError> {
        let mut ctx = self.decision_ctx(actor)?;
        let session = self.session();
        let (record, billed) = {
            let projection = self.lock_projection();
            let record = projection
                .line(order_line_id)
                .ok_or(AppError::UnknownLine)?;
            let billed = projection.bill_for_order(record.order_id).is_some();
            (record, billed)
        };
        if record.state == OrderLineState::Voided {
            return Err(AppError::UnknownLine);
        }
        // A bill names the lines it covers, frozen, when it opens (ADR-0146): a price changed under
        // it would be a total nobody quoted.
        if billed {
            return Err(AppError::BillAlreadyOpen);
        }
        if !session
            .reason_codes
            .accepts(reason_code_id, ReasonAction::PriceOverride)
        {
            return Err(AppError::ReasonCodeNotValid);
        }
        let max_multiple = session.billing.price_override_max_multiple();
        reprice_bounds(unit_price, record.added_unit_price, max_multiple)?;

        let approver = self.resolve_step_up(&mut ctx, Permission::OverridePrice, approval)?;
        decide_reprice(
            Reprice {
                unit_price,
                added_unit_price: record.added_unit_price,
                max_multiple,
                pin_verified: approver.is_some(),
            },
            &ctx,
        )?;

        let line_total = unit_price
            .mul_quantity(record.quantity, Rounding::HalfUp)
            .map_err(|error| AppError::Domain(DomainError::Money(error)))?;
        let repriced = SalesOrderLineRepriced {
            order_id: record.order_id,
            order_line_id,
            unit_price,
            previous_unit_price: record.unit_price,
            line_total,
            reason_code_id,
        };
        let (envelope, message) = self.prepare(&ctx, &repriced)?;
        let mut envelopes = vec![envelope];
        let mut messages = vec![message];
        if let Some(approver) = approver {
            let record = Self::override_record(Permission::OverridePrice, approver, None);
            let (envelope, message) = self.prepare(&ctx, &record)?;
            envelopes.push(envelope);
            messages.push(message);
        }
        self.append_and_publish(envelopes, messages).await?;

        self.lock_projection()
            .reprice_line(order_line_id, unit_price, line_total);
        Ok(repriced)
    }
}
