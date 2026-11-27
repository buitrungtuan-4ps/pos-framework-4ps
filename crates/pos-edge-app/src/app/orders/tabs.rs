// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A bar tab: a tableless order that stays open across rounds, found again by its number
//! ([ADR-0221](../../../../../docs/adr/0221-a-bar-tab-is-an-order-without-a-table-that-stays-open.md)).
//!
//! A tab's number is the business day's next queue number, given before the order commits so it
//! rides `sales.order.opened`: one daily sequence for every number a counter calls out, so a tab
//! and a takeaway never share one. A number lost to an open that then failed is a gap in the day's
//! calls and nothing more; a tab number carries no money and nothing about the guest.

use pos_core::capability::Capability;
use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::SalesChannel;

use crate::app::{AppError, CounterOrderOpened, Edge, OrderRecording, TabView};
use crate::queue::QueueNumberAuthority;

impl<S: EventStore> Edge<S> {
    /// Opens a tab: a counter order, on a store with `tabs_enabled`, numbered by `authority`.
    ///
    /// # Errors
    ///
    /// What [`Self::open_counter_order`] refuses, [`AppError::Domain`] with
    /// `CapabilityDisabled` on a store without tabs, or [`AppError::Port`] if no number can be had.
    pub async fn open_tab<A>(
        &self,
        actor: Actor,
        channel: SalesChannel,
        authority: &A,
    ) -> Result<CounterOrderOpened, AppError>
    where
        A: QueueNumberAuthority,
    {
        self.open_tab_recorded(actor, (channel, OrderRecording::default()), authority)
            .await
    }

    /// [`Self::open_tab`], recording the order type and source the till names (ADR-0260).
    ///
    /// # Errors
    ///
    /// As [`Self::open_tab`].
    pub async fn open_tab_recorded<A>(
        &self,
        actor: Actor,
        (channel, recording): (SalesChannel, OrderRecording),
        authority: &A,
    ) -> Result<CounterOrderOpened, AppError>
    where
        A: QueueNumberAuthority,
    {
        let (ctx, order_id) = self.begin_tableless(actor, channel)?;
        ctx.require_capability(Capability::Tabs)?;
        let tab_number = authority
            .allocate_queue_number(self.identity.store_id, ctx.business_date, order_id)
            .await
            .map_err(AppError::Port)?;
        self.finish_tableless(&ctx, order_id, (channel, recording), Some(tab_number))
            .await
    }

    /// Every tab still open, by number: one with no bill or a bill not yet settled, and not moved
    /// to a table.
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] if a line's tax class has no configured rate.
    pub fn open_tabs(&self) -> Result<Vec<TabView>, AppError> {
        let tabs = self.lock_projection().open_tabs();
        let mut views = Vec::with_capacity(tabs.len());
        for (order_id, tab_number, last_round_time, bill_id) in tabs {
            views.push(TabView {
                order_id,
                tab_number,
                total_due: self.order_totals(order_id)?.total_due,
                last_round_time,
                bill_id,
            });
        }
        Ok(views)
    }
}
