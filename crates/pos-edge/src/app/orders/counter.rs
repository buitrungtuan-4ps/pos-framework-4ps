// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The counter: opening a tableless order, and listing the counter orders still unpaid.

use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::events::SalesOrderOpened;
use pos_proto::text::DisplayName;
use pos_proto::{Open, SalesChannel};

use crate::app::{AppError, CounterOrderLine, CounterOrderOpened, CounterOrderView, Edge};
use crate::queue::QueueNumberAuthority;

impl<S: EventStore> Edge<S> {
    /// Opens a tableless order at the counter for the signed-in member of staff
    /// ([ADR-0146](../../../docs/adr/0146-a-counter-store-starts-its-own-orders.md)).
    ///
    /// What a store without tables was missing: until now the only way a tableless order began was
    /// the relay intake, so a kiosk could charge an order somebody else started and could not start
    /// one. It records `sales.order.opened` with no table, on a channel the store accepts, and the
    /// order is billable and settleable as every counter order has been since ADR-0093.
    ///
    /// # Errors
    ///
    /// [`AppError::Superseded`] on a replaced box, [`AppError::OpenShiftRequired`] while no shift is
    /// open at a store that refuses selling without one, [`AppError::Domain`] if the actor lacks
    /// `sales.line.add`, [`AppError::ChannelNotAccepted`] for a channel the store does not take, or
    /// [`AppError`] if the store cannot be written.
    pub async fn open_counter_order(
        &self,
        actor: Actor,
        channel: SalesChannel,
    ) -> Result<CounterOrderOpened, AppError> {
        // A replaced machine starts nothing (ADR-0123), exactly as it seats nobody.
        self.refuse_if_superseded()?;
        self.refuse_without_open_shift()?;
        let ctx = self.decision_ctx(actor)?;
        // Starting an order at the counter is taking one, as adding its first line is (ADR-0158).
        ctx.require(Permission::AddLine)?;
        if !self.session().channel_enabled(channel) {
            return Err(AppError::ChannelNotAccepted);
        }
        let order_id = self.next_order_id();
        let opened = SalesOrderOpened {
            order_id,
            channel: Open::from_known(channel),
            table_id: None,
            guest_count: None,
        };
        self.commit_and_publish(&ctx, &opened).await?;
        self.lock_projection()
            .open_order_origin(order_id, Some(channel), None);
        Ok(CounterOrderOpened {
            order_id,
            business_date: ctx.business_date,
        })
    }

    /// Every counter order still owing money, for the takeaway screen
    /// ([ADR-0093](../../../docs/adr/0093-bill-keyed-on-order.md)).
    ///
    /// The counter's equivalent of the floor plan: without it a cashier has no way to *find* a
    /// relayed order, and the routes that charge one are unreachable in practice even though they
    /// exist. Each entry carries what the order is, what it owes, the number staff shouted, and the
    /// bill already open on it if there is one.
    ///
    /// `queue_number` is read from `authority`, never allocated: minting one here would put a
    /// number on an order that never had one, every time a screen refreshed.
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] if a line's tax class has no configured rate, or [`AppError::Port`] if
    /// the queue authority cannot be read.
    pub async fn open_counter_orders<A>(
        &self,
        authority: &A,
    ) -> Result<Vec<CounterOrderView>, AppError>
    where
        A: QueueNumberAuthority,
    {
        // The ids first, with the guard released before anything awaits or re-locks.
        let order_ids = self.lock_projection().open_counter_orders();
        let session = self.session();
        let mut views = Vec::with_capacity(order_ids.len());
        for order_id in order_ids {
            let (lines, bill_id) = {
                let projection = self.lock_projection();
                (
                    projection.lines_for_order(order_id),
                    projection.bill_for_order(order_id),
                )
            };
            let items = lines
                .iter()
                .map(|line| CounterOrderLine {
                    // An item the store has since removed from its menu still has to be chargeable,
                    // so a missing entry falls back to the id rather than dropping the line and
                    // under-reporting what the guest ordered.
                    display_name: session.menu_entry(line.menu_item_id).map_or_else(
                        || DisplayName::new(line.menu_item_id.to_string()),
                        |entry| entry.display_name.clone(),
                    ),
                    quantity: line.quantity,
                })
                .collect();
            views.push(CounterOrderView {
                order_id,
                queue_number: authority
                    .queue_number_for(self.identity.store_id, order_id)
                    .await
                    .map_err(AppError::Port)?,
                items,
                total_due: self.order_totals(order_id)?.total_due,
                bill_id,
            });
        }
        Ok(views)
    }
}

#[cfg(test)]
mod tests {
    use pos_proto::ids::{MenuItemId, TableId};
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;

    use crate::app::tests::{a_counter_order, a_line, actor, cash, edge, edge_with_menu, vnd};
    use crate::queue::{InMemoryQueueNumbers, QueueNumberAuthority};

    /// An order id the edge has never seen buys nothing. The floor path proves an order exists by
    /// finding it on a table; a counter order has no table, so its lines are the proof.
    /// The counter's order list: what the takeaway screen renders
    /// ([ADR-0093](../../docs/adr/0093-bill-keyed-on-order.md)).
    ///
    /// Four properties in one test because they are one question — "which orders should a cashier
    /// see?" — and separating them would hide the interactions between them.
    #[test]
    fn the_counter_list_shows_unpaid_counter_orders_and_nothing_else() {
        pos_fakes::executor::run_ready(async {
            let edge = edge_with_menu();
            let queue = InMemoryQueueNumbers::new();

            // A floor order. It must NOT appear: the floor screen already shows it, and listing it
            // here would give staff two places to charge one meal.
            let table = TableId::new(Ulid::from_u128(306));
            edge.seat_table(actor(), table, None).await.expect("seats");
            edge.add_line(actor(), table, a_line()).await.expect("adds");

            // A counter order that has been given a queue number, as intake does.
            let waiting = a_counter_order(&edge).await;
            let number = queue
                .allocate_queue_number(
                    edge.identity.store_id,
                    edge.decision_ctx(actor()).expect("ctx").business_date,
                    waiting,
                )
                .await
                .expect("a counter order takes a number");

            // A counter order already settled. It must NOT appear: it is finished.
            let paid = a_counter_order(&edge).await;
            let bill = edge
                .open_bill_for_order(actor(), paid)
                .await
                .expect("opens");
            edge.settle_bill(actor(), bill.bill_id, vec![cash(165_000)], None)
                .await
                .expect("settles");

            // A counter order whose bill is open but unpaid. It MUST appear, carrying its bill, so
            // a screen that reloaded mid-sale resumes that bill instead of opening a second one —
            // which the domain refuses.
            let mid_sale = a_counter_order(&edge).await;
            let open_bill = edge
                .open_bill_for_order(actor(), mid_sale)
                .await
                .expect("opens");

            let listed = edge
                .open_counter_orders(&queue)
                .await
                .expect("the counter list assembles");
            let ids: Vec<_> = listed.iter().map(|view| view.order_id).collect();
            assert!(
                ids.contains(&waiting) && ids.contains(&mid_sale),
                "an unpaid counter order is listed whether or not its bill is open: {ids:?}"
            );
            assert!(
                !ids.contains(&paid),
                "a settled order is finished and off the list"
            );
            assert_eq!(
                listed.len(),
                2,
                "and the floor order is not on it — the floor screen owns that one"
            );

            let waiting_view = listed
                .iter()
                .find(|view| view.order_id == waiting)
                .expect("the waiting order is listed");
            assert_eq!(
                waiting_view.queue_number,
                Some(number),
                "the list shows the number staff actually shouted"
            );
            assert_eq!(waiting_view.total_due, vnd(165_000), "with tax, as settled");
            assert_eq!(
                waiting_view.items.len(),
                1,
                "and what is on it, for a cashier to recognise"
            );
            assert_eq!(
                waiting_view.items[0].display_name,
                DisplayName::new("Margherita")
            );
            assert_eq!(
                waiting_view.bill_id, None,
                "no bill has been opened on it yet"
            );

            let resuming = listed
                .iter()
                .find(|view| view.order_id == mid_sale)
                .expect("the mid-sale order is listed");
            assert_eq!(
                resuming.bill_id,
                Some(open_bill.bill_id),
                "the open bill is carried so the screen settles it rather than opening a second one"
            );
        });
    }

    /// An item the store has since dropped from its menu still has to be chargeable, so the list
    /// falls back to the id rather than dropping the line and under-reporting what was ordered.
    #[test]
    fn a_line_whose_item_left_the_menu_still_appears_on_the_counter_list() {
        pos_fakes::executor::run_ready(async {
            // `bootstrap()` publishes no menu at all, which is the extreme case of the same thing.
            let edge = edge();
            let queue = InMemoryQueueNumbers::new();
            a_counter_order(&edge).await;

            let listed = edge.open_counter_orders(&queue).await.expect("lists");
            assert_eq!(listed.len(), 1, "the order is still listed");
            assert_eq!(
                listed[0].items.len(),
                1,
                "and so is its line — dropping it would under-report the order"
            );
            assert_eq!(
                listed[0].items[0].display_name,
                DisplayName::new(MenuItemId::new(Ulid::from_u128(500)).to_string()),
                "with the id standing in for the name the menu no longer carries"
            );
            assert_eq!(
                listed[0].total_due,
                vnd(165_000),
                "and it is still chargeable, because a line's price was captured at add time"
            );
        });
    }

    /// Reading the list must not mint queue numbers. An order that never had one shows none.
    #[test]
    fn listing_the_counter_does_not_hand_out_a_queue_number() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let queue = InMemoryQueueNumbers::new();
            let order_id = a_counter_order(&edge).await;

            let listed = edge.open_counter_orders(&queue).await.expect("lists");
            assert_eq!(listed.len(), 1);
            assert_eq!(
                listed[0].queue_number, None,
                "an order intake never numbered stays unnumbered — a read does not allocate"
            );
            assert_eq!(
                queue
                    .queue_number_for(edge.identity.store_id, order_id)
                    .await
                    .expect("read"),
                None,
                "and the authority still holds nothing for it"
            );
        });
    }
}
