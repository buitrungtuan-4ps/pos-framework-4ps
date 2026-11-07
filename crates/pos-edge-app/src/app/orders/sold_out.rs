// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Marking a menu item sold out and restoring it, and reading what is sold out.

use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::TxContext;
use pos_ports::event_store::EventStore;
use pos_proto::events::{InventoryItemRestored, InventoryItemSoldOut};
use pos_proto::ids::MenuItemId;

use crate::app::{AppError, Edge};

impl<S: EventStore> Edge<S> {
    /// Marks an item sold out (86) at this store (`inventory.item.sold_out`), so every device greys it
    /// out and the edge refuses a line for it until somebody restores it.
    ///
    /// The kitchen runs out mid-service, and the console that publishes the menu is not where anybody
    /// is standing — so this is an in-store override on top of the published menu's own
    /// availability, recorded as the event `pos-proto` has always carried for it, with `automatic`
    /// false: a person decided, not a stock threshold. [`Permission::MarkItemUnavailable`], which a
    /// cook, a server and a cashier hold by default and which asks for no PIN: running out of
    /// something is a fact, and it moves no money.
    ///
    /// Marking an item already sold out writes nothing, so a device that taps it after another has
    /// records no second decision. Two taps landing in the same instant can both write, as any
    /// command here can; the fold is a set, so the item is sold out exactly as it is after one.
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] if the actor lacks the permission, [`AppError::ItemNotSellable`] if the
    /// store's menu has no such item, or [`AppError`] if the store cannot be written.
    pub async fn mark_item_sold_out(
        &self,
        actor: Actor,
        menu_item_id: MenuItemId,
    ) -> Result<(), AppError> {
        self.set_item_sold_out(actor, menu_item_id, true).await
    }

    /// Restores an item staff marked sold out (`inventory.item.restored`). The same permission, the
    /// same idempotence: restoring an item nobody marked writes nothing.
    ///
    /// # Errors
    ///
    /// As [`Self::mark_item_sold_out`].
    pub async fn restore_item(
        &self,
        actor: Actor,
        menu_item_id: MenuItemId,
    ) -> Result<(), AppError> {
        self.set_item_sold_out(actor, menu_item_id, false).await
    }

    /// Whether staff have marked an item sold out at this store.
    #[must_use]
    pub fn item_sold_out(&self, menu_item_id: MenuItemId) -> bool {
        self.lock_projection().sold_out.contains(&menu_item_id)
    }

    /// The first of an item and its chosen modifiers that staff have marked sold out, if any.
    ///
    /// Every path that adds a line asks this — the table's, the counter's, and a guest's or a
    /// marketplace's order (`crate::order_in`) — and asks it of the modifiers too: a modifier is an
    /// item, and the extra cheese runs out as surely as the pizza does.
    #[must_use]
    pub fn first_sold_out(
        &self,
        menu_item_id: MenuItemId,
        modifier_menu_item_ids: &[MenuItemId],
    ) -> Option<MenuItemId> {
        let projection = self.lock_projection();
        std::iter::once(&menu_item_id)
            .chain(modifier_menu_item_ids)
            .find(|id| projection.sold_out.contains(id))
            .copied()
    }

    async fn set_item_sold_out(
        &self,
        actor: Actor,
        menu_item_id: MenuItemId,
        sold_out: bool,
    ) -> Result<(), AppError> {
        let ctx = self.decision_ctx(actor)?;
        ctx.require(Permission::MarkItemUnavailable)?;
        if self.session().menu_entry(menu_item_id).is_none() {
            return Err(AppError::ItemNotSellable);
        }
        if self.item_sold_out(menu_item_id) == sold_out {
            return Ok(());
        }
        let (envelope, message) = if sold_out {
            let payload = InventoryItemSoldOut {
                menu_item_id,
                automatic: false,
            };
            self.prepare(&ctx, &payload)?
        } else {
            self.prepare(&ctx, &InventoryItemRestored { menu_item_id })?
        };
        let mut tx = self.store.begin().await?;
        self.store.append(&mut tx, &[envelope]).await?;
        tx.commit().await?;
        // Folded before any device is told, not after: a till that hears an item brought back reads
        // the price book again at once, and a read served between the two answered the item still
        // sold out, under a new tag, with nothing left to read it again. That till kept the item
        // greyed out while every other till sold it.
        self.lock_projection().set_sold_out(menu_item_id, sold_out);
        self.fanout.publish(&message);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pos_proto::ids::MenuItemId;
    use pos_proto::ulid::Ulid;

    use crate::app::tests::{actor, edge_with_menu};

    // The fan-out is held, so the mark is committed and no device has heard it yet: whatever a
    // device then reads must already answer the mark, both ways.
    #[test]
    fn a_device_that_hears_a_mark_reads_it_back() {
        let edge = Arc::new(edge_with_menu());
        let item = MenuItemId::new(Ulid::from_u128(500));
        let mut device = edge.fanout().subscribe();
        for sold_out in [true, false] {
            let held = edge.fanout().hold();
            let writer = {
                let edge = Arc::clone(&edge);
                std::thread::spawn(move || {
                    pos_fakes::executor::run_ready(async {
                        if sold_out {
                            edge.mark_item_sold_out(actor(), item).await
                        } else {
                            edge.restore_item(actor(), item).await
                        }
                    })
                })
            };
            // Bounded by tries rather than by a clock: the writer needs a handful.
            let answered = (0..10_000_000).any(|_| {
                std::thread::yield_now();
                edge.item_sold_out(item) == sold_out
            });
            assert!(
                answered,
                "the store answers the mark only once a device has been told of it"
            );
            assert!(device.try_recv().is_err(), "no device has heard it yet");
            drop(held);
            writer
                .join()
                .expect("the writer ran")
                .expect("the mark is recorded");
            assert!(device.try_recv().is_ok(), "then every device hears it");
        }
    }
}
