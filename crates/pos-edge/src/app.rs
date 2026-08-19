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
//! ([ADR-0013](../../../docs/adr/0013-async-strategy.md)). This slice wires the **table floor cycle**
//! (seat and clean, the two evented table transitions); the order, bill and shift families follow the
//! identical shape.

use std::collections::HashMap;
use std::sync::Mutex;

use pos_core::business_date::{CutoffHour, StoreTimeZone, derive_business_date};
use pos_core::campaign::Connectivity;
use pos_core::capability::CapabilityContext;
use pos_core::decision::{Actor, DecisionCtx, TableCommand, decide_table};
use pos_core::error::DomainError;
use pos_core::permission::PermissionSet;
use pos_ports::event_store::EventStore;
use pos_ports::{PortError, TxContext};
use pos_proto::envelope::{DecodeError, EventEnvelope, EventPayload, EventTypeRef, RawPayload};
use pos_proto::events::{SalesTableClosed, SalesTableOpened};
use pos_proto::ids::{BrandId, StoreId, TableId, TenantId};
use pos_proto::money::CurrencyCode;
use pos_proto::{ClockSource, IdGenerator, TableState};

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
}

/// What a table looks like to a caller after a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableView {
    /// The table.
    pub table_id: TableId,
    /// Its state after the command.
    pub state: TableState,
}

/// The in-memory projection: the current state of each aggregate, folded from the event log.
///
/// Durable truth is the event log in the store; this is a cache rebuilt at boot and updated as
/// decisions apply, so a screen is answered without replaying events per request. This slice tracks
/// table states; other aggregates join it as their flows land.
#[derive(Debug, Default)]
struct Projection {
    tables: HashMap<TableId, TableState>,
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
        self.apply(&ctx, &payload, decision.next_state, table_id)
            .await
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
        self.apply(&ctx, &payload, decision.next_state, table_id)
            .await
    }

    /// The commit-and-publish half of the loop, shared by every table command: write the event in one
    /// transaction, then fold it into the projection and tell every device — in that order, so a
    /// rolled-back write is never seen.
    async fn apply<P: EventPayload + serde::Serialize>(
        &self,
        ctx: &DecisionCtx,
        payload: &P,
        next_state: TableState,
        table_id: TableId,
    ) -> Result<TableView, AppError> {
        let published = serde_json::to_value(payload).unwrap_or(serde_json::Value::Null);
        let event_type = EventTypeRef::from_known(P::EVENT_TYPE).as_str().to_owned();
        let envelope = self.envelope(ctx, payload)?;

        let mut tx = self.store.begin().await?;
        self.store
            .append(&mut tx, std::slice::from_ref(&envelope))
            .await?;
        tx.commit().await?;

        // After the commit: the change is durable, so it is safe to show.
        self.projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_table(table_id, next_state);
        self.fanout.publish(&ServerMessage::Event {
            event_type,
            payload: published,
        });

        Ok(TableView {
            table_id,
            state: next_state,
        })
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

    fn next_ulid(&self) -> pos_proto::ulid::Ulid {
        self.ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .next_id()
    }

    fn next_order_id(&self) -> pos_proto::ids::OrderId {
        pos_proto::ids::OrderId::new(self.next_ulid())
    }
}

#[cfg(test)]
mod tests {
    use super::{Edge, EdgeSession, StoreIdentity};
    use pos_core::business_date::{CutoffHour, StoreTimeZone};
    use pos_core::campaign::Connectivity;
    use pos_core::capability::CapabilityContext;
    use pos_core::decision::Actor;
    use pos_core::permission::{Permission, PermissionSet};
    use pos_fakes::FakeStore;
    use pos_proto::TableState;
    use pos_proto::ids::{BrandId, DeviceId, EmployeeId, StoreId, TableId, TenantId};
    use pos_proto::money::CurrencyCode;
    use pos_proto::ulid::Ulid;

    fn identity() -> StoreIdentity {
        StoreIdentity {
            tenant_id: TenantId::new(Ulid::from_u128(1)),
            brand_id: BrandId::new(Ulid::from_u128(2)),
            store_id: StoreId::new(Ulid::from_u128(3)),
        }
    }

    fn full_session() -> EdgeSession {
        EdgeSession {
            granted: Permission::ALL.iter().copied().collect::<PermissionSet>(),
            capabilities: CapabilityContext::full_service(),
            currency: CurrencyCode::VND,
            timezone: StoreTimeZone::utc(),
            cutoff: CutoffHour::new(4).expect("valid cutoff"),
            connectivity: Connectivity::Offline,
        }
    }

    fn actor() -> Actor {
        Actor {
            employee_id: EmployeeId::new(Ulid::from_u128(10)),
            device_id: DeviceId::new(Ulid::from_u128(20)),
        }
    }

    fn edge() -> Edge<FakeStore> {
        Edge::new(FakeStore::default(), identity(), full_session()).expect("seeds")
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
}
