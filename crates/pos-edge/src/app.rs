// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The application layer: load → decide → apply → publish.
//!
//! This is the thin layer [ADR-0013](../../../docs/adr/0013-async-strategy.md) says each binary
//! owns. For every command it: **loads** the aggregate's current state from the projection,
//! **decides** with the synchronous `pos-core` spine ([`decide_table`] and its siblings), **applies**
//! the decision by writing the wire events it maps to inside one store transaction, and — only after
//! the commit — **publishes** the change to every device over the fan-out and folds it into the
//! projection. A rolled-back transaction therefore prints nothing, tells no device, and changes no
//! screen.
//!
//! [`Edge`] is generic over the store `S`, so the same loop runs against `pos-fakes` in a test and
//! `store-sqlite` on a real machine — static dispatch, no `dyn`
//! ([ADR-0013](../../../docs/adr/0013-async-strategy.md)). It wires the **table floor cycle** (seat,
//! clean) and the **order line** (add, fire); the bill and shift families follow the identical shape.

use std::collections::HashMap;
use std::sync::Mutex;

use pos_core::business_date::{CutoffHour, StoreTimeZone, derive_business_date};
use pos_core::campaign::Connectivity;
use pos_core::capability::CapabilityContext;
use pos_core::decision::{
    Actor, DecisionCtx, LineCommand, TableCommand, decide_line, decide_table,
};
use pos_core::error::DomainError;
use pos_core::inventory::RecipeBook;
use pos_core::permission::{Permission, PermissionSet};
use pos_ports::event_store::EventStore;
use pos_ports::{PortError, TxContext};
use pos_proto::envelope::{DecodeError, EventEnvelope, EventPayload, EventTypeRef, RawPayload};
use pos_proto::events::{
    SalesOrderLineAdded, SalesOrderLineFired, SalesTableClosed, SalesTableOpened,
};
use pos_proto::ids::{
    BrandId, CourseId, MenuItemId, OrderId, OrderLineId, StationId, StoreId, TableId, TaxClassId,
    TenantId,
};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{ClockSource, IdGenerator, OrderLineState, TableState};

use crate::clock::SystemClock;
use crate::fanout::{Fanout, ServerMessage};
use crate::idgen::EdgeIdGenerator;

/// Which tenant, brand and store this edge is — the envelope context every event carries. Assigned
/// at activation ([ADR-0003](../../../docs/adr/0003-cattle-not-pets.md)); all three are identifiers,
/// not PII.
#[derive(Debug, Clone, Copy)]
pub struct StoreIdentity {
    /// Owning tenant.
    pub tenant_id: TenantId,
    /// Owning brand.
    pub brand_id: BrandId,
    /// This store.
    pub store_id: StoreId,
}

impl StoreIdentity {
    /// The identity for a store, with bootstrap tenant and brand ids until activation
    /// ([ADR-0003](../../../docs/adr/0003-cattle-not-pets.md)) supplies the real ones.
    #[must_use]
    pub fn for_store(store_id: StoreId) -> Self {
        Self {
            tenant_id: TenantId::new(Ulid::from_u128(1)),
            brand_id: BrandId::new(Ulid::from_u128(1)),
            store_id,
        }
    }
}

/// The session defaults a decision reads — normally the store's synced configuration
/// ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md)). Held here so the decision spine
/// is config-driven; the values arrive from the cloud config tree in P7.
#[derive(Debug, Clone)]
pub struct EdgeSession {
    /// What the acting role is granted (§9).
    pub granted: PermissionSet,
    /// The store's capability profile (§10).
    pub capabilities: CapabilityContext,
    /// The store's currency.
    pub currency: CurrencyCode,
    /// The store's timezone, for business-date derivation (ADR-0014).
    pub timezone: StoreTimeZone,
    /// The hour the trading day rolls over.
    pub cutoff: CutoffHour,
    /// Whether the store is currently online (drives the campaign engine's offline rule).
    pub connectivity: Connectivity,
    /// The recipes a fire consumes (§8). Empty in the bootstrap: an item with no recipe consumes
    /// nothing, so the flow runs before the menu's bill of materials is synced (P7).
    pub recipes: RecipeBook,
}

impl EdgeSession {
    /// Bootstrap defaults until the cloud config tree ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md),
    /// P7) supplies the real values: a full-service store, every permission granted, VND, UTC with a
    /// 04:00 cut-off, offline. Enough for the edge to sell on fakes and for the dine-in flow to run.
    #[must_use]
    #[expect(
        clippy::missing_panics_doc,
        reason = "the only expect is on CutoffHour::new(4), a compile-time constant that is always valid"
    )]
    pub fn bootstrap() -> Self {
        Self {
            granted: Permission::ALL.iter().copied().collect(),
            capabilities: CapabilityContext::full_service(),
            currency: CurrencyCode::VND,
            timezone: StoreTimeZone::utc(),
            cutoff: CutoffHour::new(4).expect("4 is a valid cut-off hour"),
            connectivity: Connectivity::Offline,
            recipes: RecipeBook::default(),
        }
    }
}

/// A failure applying a command.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The domain refused the command — a bad transition, a missing permission, a disabled capability.
    #[error("the command was refused: {0}")]
    Domain(#[from] DomainError),
    /// The store could not be read or written.
    #[error("the store failed: {0}")]
    Port(#[from] PortError),
    /// The business date could not be derived from the clock and the store's timezone.
    #[error("could not derive the business date")]
    Clock,
    /// An event payload could not be encoded to JSON.
    #[error("could not encode an event: {0}")]
    Encode(DecodeError),
    /// A line was added to a table that has no open order — it must be seated first.
    #[error("the table has no open order")]
    NoOpenOrder,
    /// A command named a line the edge does not know.
    #[error("no such order line")]
    UnknownLine,
}

/// What a table looks like to a caller after a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableView {
    /// The table.
    pub table_id: TableId,
    /// Its state after the command.
    pub state: TableState,
}

/// A line as the caller wants it added — the amounts captured from the menu the device holds
/// (`sales.order_line.added` §: a line never references the live menu, so these are recorded now and
/// never looked up later). The edge does not invent prices; it records what the device supplies.
#[derive(Debug, Clone)]
pub struct LineDraft {
    /// The menu item, for reporting.
    pub menu_item_id: MenuItemId,
    /// The name shown to the guest at this moment.
    pub display_name: DisplayName,
    /// How many.
    pub quantity: Quantity,
    /// Unit price at this moment.
    pub unit_price: Money,
    /// Extended total at this moment.
    pub line_total: Money,
    /// Tax class at this moment.
    pub tax_class_id: TaxClassId,
    /// Tax rate in force at this moment.
    pub tax_rate: Ratio,
    /// Seat, when seats are enabled.
    pub seat: Option<u16>,
    /// Course, when courses are enabled.
    pub course_id: Option<CourseId>,
    /// Whether a guest note was written — its text never enters the log.
    pub note_present: bool,
}

/// What a line looks like to a caller after a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineView {
    /// The order the line belongs to.
    pub order_id: OrderId,
    /// The line.
    pub order_line_id: OrderLineId,
    /// Its state after the command.
    pub state: OrderLineState,
}

/// What the projection remembers about one order line, so a fire can be decided and its consumption
/// computed without re-reading the event log.
#[derive(Debug, Clone, Copy)]
struct LineRecord {
    order_id: OrderId,
    state: OrderLineState,
    menu_item_id: MenuItemId,
    quantity: Quantity,
    course_id: Option<CourseId>,
}

/// The in-memory projection: the current state of each aggregate, folded from the event log.
///
/// Durable truth is the event log in the store; this is a cache rebuilt at boot and updated as
/// decisions apply, so a screen is answered without replaying events per request. This slice tracks
/// tables, the order each table holds, and each line's state; the bill and shift aggregates join it
/// as their flows land.
#[derive(Debug, Default)]
struct Projection {
    tables: HashMap<TableId, TableState>,
    table_orders: HashMap<TableId, OrderId>,
    lines: HashMap<OrderLineId, LineRecord>,
}

impl Projection {
    fn table_state(&self, table_id: TableId) -> TableState {
        self.tables
            .get(&table_id)
            .copied()
            .unwrap_or(TableState::Free)
    }

    fn set_table(&mut self, table_id: TableId, state: TableState) {
        self.tables.insert(table_id, state);
    }

    fn open_order(&mut self, table_id: TableId, order_id: OrderId) {
        self.table_orders.insert(table_id, order_id);
    }

    fn order_for_table(&self, table_id: TableId) -> Option<OrderId> {
        self.table_orders.get(&table_id).copied()
    }

    fn add_line(&mut self, line_id: OrderLineId, record: LineRecord) {
        self.lines.insert(line_id, record);
    }

    fn line(&self, line_id: OrderLineId) -> Option<LineRecord> {
        self.lines.get(&line_id).copied()
    }

    fn set_line_state(&mut self, line_id: OrderLineId, state: OrderLineState) {
        if let Some(record) = self.lines.get_mut(&line_id) {
            record.state = state;
        }
    }
}

/// The edge application: the store, the session, and the loop that ties them to `pos-core`.
#[derive(Debug)]
pub struct Edge<S> {
    store: S,
    identity: StoreIdentity,
    session: EdgeSession,
    clock: SystemClock,
    ids: Mutex<EdgeIdGenerator<SystemClock>>,
    fanout: Fanout,
    projection: Mutex<Projection>,
}

impl<S: EventStore> Edge<S> {
    /// Composes an edge over `store` for a given identity and session.
    ///
    /// # Errors
    ///
    /// [`getrandom::Error`] if the OS entropy source needed to seed the id generator is unavailable.
    pub fn new(
        store: S,
        identity: StoreIdentity,
        session: EdgeSession,
    ) -> Result<Self, getrandom::Error> {
        Ok(Self {
            store,
            identity,
            session,
            clock: SystemClock,
            ids: Mutex::new(EdgeIdGenerator::new(SystemClock)?),
            fanout: Fanout::new(),
            projection: Mutex::new(Projection::default()),
        })
    }

    /// The fan-out devices subscribe to.
    #[must_use]
    pub fn fanout(&self) -> &Fanout {
        &self.fanout
    }

    /// The current projected state of a table.
    #[must_use]
    pub fn table_state(&self, table_id: TableId) -> TableState {
        self.projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .table_state(table_id)
    }

    /// Seats guests at a table, opening an order on it (`sales.table.opened`).
    ///
    /// # Errors
    ///
    /// [`AppError`] if the transition is illegal, the store cannot be written, or the business date
    /// cannot be derived.
    pub async fn seat_table(&self, actor: Actor, table_id: TableId) -> Result<TableView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let current = self.table_state(table_id);
        let decision = decide_table(current, TableCommand::Seat, &ctx)?;

        let order_id = self.next_order_id();
        let payload = SalesTableOpened { table_id, order_id };
        self.commit_and_publish(&ctx, &payload).await?;

        // After the commit the change is durable, so it is safe to show and to remember.
        {
            let mut projection = self.lock_projection();
            projection.set_table(table_id, decision.next_state);
            projection.open_order(table_id, order_id);
        }
        Ok(TableView {
            table_id,
            state: decision.next_state,
        })
    }

    /// Cleans a table down and releases it (`sales.table.closed`).
    ///
    /// # Errors
    ///
    /// [`AppError`] as [`Self::seat_table`].
    pub async fn clean_table(
        &self,
        actor: Actor,
        table_id: TableId,
    ) -> Result<TableView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let current = self.table_state(table_id);
        let decision = decide_table(current, TableCommand::Clean, &ctx)?;

        let payload = SalesTableClosed { table_id };
        self.commit_and_publish(&ctx, &payload).await?;
        self.lock_projection()
            .set_table(table_id, decision.next_state);
        Ok(TableView {
            table_id,
            state: decision.next_state,
        })
    }

    /// The current state of a line, if the edge knows it.
    #[must_use]
    pub fn line_state(&self, order_line_id: OrderLineId) -> Option<OrderLineState> {
        self.lock_projection().line(order_line_id).map(|r| r.state)
    }

    /// Adds a line to the order the table holds (`sales.order_line.added`).
    ///
    /// Adding a line is not a state-machine transition and needs no permission; it records what the
    /// device captured. A new line starts [`OrderLineState::Added`].
    ///
    /// # Errors
    ///
    /// [`AppError::NoOpenOrder`] if the table has not been seated, or [`AppError`] if the store cannot
    /// be written.
    pub async fn add_line(
        &self,
        actor: Actor,
        table_id: TableId,
        draft: LineDraft,
    ) -> Result<LineView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let order_id = self
            .lock_projection()
            .order_for_table(table_id)
            .ok_or(AppError::NoOpenOrder)?;
        let order_line_id = OrderLineId::new(self.next_ulid());

        let payload = SalesOrderLineAdded {
            order_id,
            order_line_id,
            menu_item_id: draft.menu_item_id,
            display_name: draft.display_name,
            quantity: draft.quantity,
            unit_price: draft.unit_price,
            line_total: draft.line_total,
            tax_class_id: draft.tax_class_id,
            tax_rate: draft.tax_rate,
            seat: draft.seat,
            course_id: draft.course_id,
            note_present: draft.note_present,
        };
        self.commit_and_publish(&ctx, &payload).await?;

        self.lock_projection().add_line(
            order_line_id,
            LineRecord {
                order_id,
                state: OrderLineState::Added,
                menu_item_id: draft.menu_item_id,
                quantity: draft.quantity,
                course_id: draft.course_id,
            },
        );
        Ok(LineView {
            order_id,
            order_line_id,
            state: OrderLineState::Added,
        })
    }

    /// Fires a line to the kitchen (`sales.order_line.fired`), consuming its recipe (§8).
    ///
    /// The consumption `pos-core` computes is a stock-ledger concern applied by the projection (a
    /// later slice); with the bootstrap empty recipe book there is nothing to consume, and the fire
    /// event is the durable record that stock left at fire, not at payment.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownLine`] for a line the edge does not know, [`AppError::Domain`] if firing is
    /// not a legal move (already fired or voided) or the course capability is off, or [`AppError`] if
    /// the store cannot be written.
    pub async fn fire_line(
        &self,
        actor: Actor,
        order_line_id: OrderLineId,
        station_id: StationId,
    ) -> Result<LineView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let record = self
            .lock_projection()
            .line(order_line_id)
            .ok_or(AppError::UnknownLine)?;

        let command = LineCommand::Fire {
            base_item: record.menu_item_id,
            modifiers: Vec::new(),
            quantity: record.quantity,
            course: record.course_id,
        };
        let decision = decide_line(record.state, command, &ctx, &self.session.recipes)?;

        let payload = SalesOrderLineFired {
            order_id: record.order_id,
            order_line_id,
            station_id,
            fire_time: ctx.now,
        };
        self.commit_and_publish(&ctx, &payload).await?;

        self.lock_projection()
            .set_line_state(order_line_id, decision.next_state);
        Ok(LineView {
            order_id: record.order_id,
            order_line_id,
            state: decision.next_state,
        })
    }

    /// The commit-and-publish half of every command: write the event in one transaction, then — and
    /// only then — tell every device. In that order, so a rolled-back write is never seen. Updating
    /// the projection is the caller's, done after this returns for the same reason.
    async fn commit_and_publish<P: EventPayload + serde::Serialize>(
        &self,
        ctx: &DecisionCtx,
        payload: &P,
    ) -> Result<(), AppError> {
        let published = serde_json::to_value(payload).unwrap_or(serde_json::Value::Null);
        let event_type = EventTypeRef::from_known(P::EVENT_TYPE).as_str().to_owned();
        let envelope = self.envelope(ctx, payload)?;

        let mut tx = self.store.begin().await?;
        self.store
            .append(&mut tx, std::slice::from_ref(&envelope))
            .await?;
        tx.commit().await?;

        self.fanout.publish(&ServerMessage::Event {
            event_type,
            payload: published,
        });
        Ok(())
    }

    /// Locks the projection, recovering from a poisoned lock rather than propagating the panic.
    fn lock_projection(&self) -> std::sync::MutexGuard<'_, Projection> {
        self.projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Builds the wire envelope for a typed payload, stamping the full context.
    fn envelope<P: EventPayload>(
        &self,
        ctx: &DecisionCtx,
        payload: &P,
    ) -> Result<EventEnvelope<RawPayload>, AppError> {
        let data = RawPayload::encode(payload).map_err(AppError::Encode)?;
        Ok(EventEnvelope {
            event_id: pos_proto::ids::EventId::new(self.next_ulid()),
            event_type: EventTypeRef::from_known(P::EVENT_TYPE),
            event_time: ctx.now,
            business_date: ctx.business_date,
            schema_version: P::SCHEMA_VERSION,
            tenant_id: self.identity.tenant_id,
            brand_id: self.identity.brand_id,
            store_id: self.identity.store_id,
            device_id: ctx.actor.device_id,
            employee_id: Some(ctx.actor.employee_id),
            shift_id: None,
            data,
        })
    }

    /// Assembles the decision context from the clock and the session.
    fn decision_ctx(&self, actor: Actor) -> Result<DecisionCtx, AppError> {
        let now = self.clock.now();
        let business_date = derive_business_date(now, &self.session.timezone, self.session.cutoff)
            .map_err(|_| AppError::Clock)?;
        Ok(DecisionCtx {
            now,
            business_date,
            actor,
            granted: self.session.granted,
            capabilities: self.session.capabilities,
            connectivity: self.session.connectivity,
            currency: self.session.currency,
        })
    }

    fn next_ulid(&self) -> Ulid {
        self.ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .next_id()
    }

    fn next_order_id(&self) -> OrderId {
        OrderId::new(self.next_ulid())
    }
}

#[cfg(test)]
mod tests {
    use super::{Edge, EdgeSession, LineDraft, StoreIdentity};
    use pos_core::decision::Actor;
    use pos_fakes::FakeStore;
    use pos_proto::ids::{
        DeviceId, EmployeeId, MenuItemId, StationId, StoreId, TableId, TaxClassId,
    };
    use pos_proto::money::{CurrencyCode, Money, Ratio};
    use pos_proto::quantity::Quantity;
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;
    use pos_proto::{OrderLineState, TableState};

    fn identity() -> StoreIdentity {
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(3)))
    }

    fn actor() -> Actor {
        Actor {
            employee_id: EmployeeId::new(Ulid::from_u128(10)),
            device_id: DeviceId::new(Ulid::from_u128(20)),
        }
    }

    fn edge() -> Edge<FakeStore> {
        Edge::new(FakeStore::default(), identity(), EdgeSession::bootstrap()).expect("seeds")
    }

    fn a_line() -> LineDraft {
        LineDraft {
            menu_item_id: MenuItemId::new(Ulid::from_u128(500)),
            display_name: DisplayName::new("Margherita"),
            quantity: Quantity::ONE,
            unit_price: Money::new(CurrencyCode::VND, 150_000),
            line_total: Money::new(CurrencyCode::VND, 150_000),
            tax_class_id: TaxClassId::new(Ulid::from_u128(1)),
            tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
            seat: None,
            course_id: None,
            note_present: false,
        }
    }

    #[test]
    fn seating_a_table_opens_it() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(100));

            assert_eq!(edge.table_state(table), TableState::Free);
            let view = edge.seat_table(actor(), table).await.expect("seats");
            assert_eq!(view.state, TableState::Occupied);
            assert_eq!(edge.table_state(table), TableState::Occupied);
        });
    }

    #[test]
    fn a_second_device_sees_the_change_over_the_fanout() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(101));
            let mut device_a = edge.fanout().subscribe();
            let mut device_b = edge.fanout().subscribe();

            edge.seat_table(actor(), table).await.expect("seats");

            for device in [&mut device_a, &mut device_b] {
                let frame = device.try_recv().expect("a frame reached the device");
                assert!(frame.contains("sales.table.opened"));
            }
        });
    }

    #[test]
    fn an_illegal_transition_is_refused_and_nothing_is_published() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(102));
            let mut device = edge.fanout().subscribe();

            // Cleaning a Free table is not a legal move (clean is needs-cleaning -> free).
            let refused = edge.clean_table(actor(), table).await;
            assert!(refused.is_err(), "an illegal transition is refused");
            assert!(
                device.try_recv().is_err(),
                "a refused command publishes nothing"
            );
            assert_eq!(
                edge.table_state(table),
                TableState::Free,
                "state is unchanged"
            );
        });
    }

    #[test]
    fn a_line_is_added_to_a_seated_table_then_fired() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(200));
            edge.seat_table(actor(), table).await.expect("seats");

            let line = edge.add_line(actor(), table, a_line()).await.expect("adds");
            assert_eq!(line.state, OrderLineState::Added);
            assert_eq!(
                edge.line_state(line.order_line_id),
                Some(OrderLineState::Added)
            );

            let station = StationId::new(Ulid::from_u128(9));
            let fired = edge
                .fire_line(actor(), line.order_line_id, station)
                .await
                .expect("fires");
            assert_eq!(fired.state, OrderLineState::Fired);
            assert_eq!(
                edge.line_state(line.order_line_id),
                Some(OrderLineState::Fired)
            );
        });
    }

    #[test]
    fn a_line_on_an_unseated_table_is_refused() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(201));
            // Never seated: there is no open order to add to.
            let refused = edge.add_line(actor(), table, a_line()).await;
            assert!(matches!(refused, Err(super::AppError::NoOpenOrder)));
        });
    }

    #[test]
    fn firing_an_already_fired_line_is_refused() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(202));
            edge.seat_table(actor(), table).await.expect("seats");
            let line = edge.add_line(actor(), table, a_line()).await.expect("adds");
            let station = StationId::new(Ulid::from_u128(9));
            edge.fire_line(actor(), line.order_line_id, station)
                .await
                .expect("first fire");

            // A fired line cannot fire again — the domain refuses the transition.
            let refused = edge.fire_line(actor(), line.order_line_id, station).await;
            assert!(matches!(refused, Err(super::AppError::Domain(_))));
        });
    }
}
