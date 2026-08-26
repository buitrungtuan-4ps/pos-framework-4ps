// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge order-relay client ([ADR-0061](../../../docs/adr/0061-order-relay.md)): pull the cloud
//! queue, make each order through the real `EdgeOrderIn`, ack the outcome.
//!
//! The transport is faked (no socket); the intake is the genuine `EdgeOrderIn` over the in-memory
//! store, so a pulled order really reprices, opens in the log, and dedupes — the same path a guest QR
//! order or a marketplace order takes. Two things are proven: the pull→make→ack round trip lands one
//! order and reports it Accepted, and the re-declared wire shapes deserialize the cloud's JSON.

use std::sync::{Arc, Mutex};

use pos_edge::relay_client::{
    PendingOrderDto, QueuedOrderLine, QueuedOrderPayload, RelayClient, RelayTransport,
    RelayTransportError, StoreOutcome,
};
use pos_edge::{
    Edge, EdgeOrderIn, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_proto::SalesChannel;
use pos_proto::ids::{DeviceId, MenuItemId, StoreId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;

fn store() -> StoreId {
    StoreId::new(Ulid::from_u128(7))
}

fn known_item() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(500))
}

/// A fresh `EdgeOrderIn` over the in-memory store, seeded with one item at 120 000 VND on the
/// standard class (taxed on every channel), the same seed the `OrderIn` suite uses.
fn intake() -> EdgeOrderIn<FakeStore, InMemoryQueueNumbers> {
    let class = EdgeSession::standard_tax_class();
    let menu = MenuCatalog::new().with(MenuEntry::new(
        known_item(),
        DisplayName::new("Margherita"),
        Money::new(CurrencyCode::VND, 120_000),
        class,
    ));
    let rates = TaxRateTable::new()
        .with(class, SalesChannel::DineIn, TaxRate::from_percent(10))
        .with(class, SalesChannel::Qr, TaxRate::from_percent(10));
    let session = EdgeSession::bootstrap()
        .with_menu(menu)
        .with_tax_rates(rates);
    let edge = Edge::new(
        FakeStore::default(),
        StoreIdentity::for_store(store()),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator");
    EdgeOrderIn::new(
        Arc::new(edge),
        InMemoryQueueNumbers::new(),
        DeviceId::new(Ulid::from_u128(20)),
    )
}

/// An in-memory relay transport: it hands out its pending orders on pull and removes an order from
/// the pending set when it is acked, so a re-pull after a full pump is empty — the cloud's
/// at-least-once-until-acked contract, faked.
#[derive(Clone, Default)]
struct FakeTransport {
    pending: Arc<Mutex<Vec<PendingOrderDto>>>,
    acks: Arc<Mutex<Vec<(String, StoreOutcome)>>>,
}

impl FakeTransport {
    fn with(orders: Vec<PendingOrderDto>) -> Self {
        Self {
            pending: Arc::new(Mutex::new(orders)),
            acks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn acks(&self) -> Vec<(String, StoreOutcome)> {
        self.acks.lock().expect("acks lock").clone()
    }
}

impl RelayTransport for FakeTransport {
    async fn pull(&self) -> Result<Vec<PendingOrderDto>, RelayTransportError> {
        Ok(self.pending.lock().expect("pending lock").clone())
    }

    async fn ack(
        &self,
        queued_id: &str,
        outcome: &StoreOutcome,
    ) -> Result<(), RelayTransportError> {
        self.pending
            .lock()
            .expect("pending lock")
            .retain(|entry| entry.queued_id != queued_id);
        self.acks
            .lock()
            .expect("acks lock")
            .push((queued_id.to_owned(), outcome.clone()));
        Ok(())
    }
}

/// One pending QR order for the seeded item, at a table.
fn pending_qr_order(queued_id: &str, reference: &str) -> PendingOrderDto {
    PendingOrderDto {
        queued_id: queued_id.to_owned(),
        order: QueuedOrderPayload {
            external_reference: reference.to_owned(),
            sales_channel: "SALES_CHANNEL_QR".to_owned(),
            store_id: store().to_string(),
            table_id: Some(Ulid::from_u128(0x7AB1E).to_string()),
            subject_id: None,
            lines: vec![QueuedOrderLine {
                menu_item_id: known_item().to_string(),
                quantity_milli: 1000,
                modifier_menu_item_ids: Vec::new(),
                quoted_unit_price: None,
                note: None,
            }],
            placed_at_ms: 1_700_000_000_000,
        },
    }
}

#[test]
fn a_pulled_order_is_made_and_acked_accepted() {
    let transport = FakeTransport::with(vec![pending_qr_order("q-1", "relay-1")]);
    let client = RelayClient::new(transport.clone(), intake());

    let processed = run_ready(client.pump_once()).expect("pump");
    assert_eq!(processed, 1, "one pulled order was processed");

    let acks = transport.acks();
    assert_eq!(acks.len(), 1);
    let (queued_id, outcome) = &acks[0];
    assert_eq!(queued_id, "q-1");
    match outcome {
        StoreOutcome::Accepted(record) => {
            assert!(record.created, "the store created the order");
            assert_eq!(
                record.total.amount_minor, 120_000,
                "the store's own price is authoritative"
            );
            assert!(
                record.awaiting_staff_confirmation,
                "a QR (table) order awaits staff confirmation"
            );
        }
        StoreOutcome::Rejected { status, message } => {
            panic!("expected acceptance, got rejected: {status} {message}");
        }
    }

    // Once acked, the order is gone from the queue: a second pump processes nothing.
    let again = run_ready(client.pump_once()).expect("second pump");
    assert_eq!(again, 0, "the acked order is not re-pulled");
}

#[test]
fn a_malformed_payload_is_acked_as_invalid_argument_not_dropped() {
    let mut bad = pending_qr_order("q-bad", "relay-bad");
    bad.order.lines[0].menu_item_id = "not-a-ulid".to_owned();
    let transport = FakeTransport::with(vec![bad]);
    let client = RelayClient::new(transport.clone(), intake());

    let processed = run_ready(client.pump_once()).expect("pump");
    assert_eq!(processed, 1);
    let acks = transport.acks();
    match &acks[0].1 {
        StoreOutcome::Rejected { status, .. } => assert_eq!(status, "invalid_argument"),
        StoreOutcome::Accepted(_) => panic!("a malformed order must not be accepted"),
    }
}

#[test]
fn the_wire_shapes_deserialize_the_clouds_pull_json() {
    // A batch shaped exactly as `pos_cloud::relay`'s `PendingOrderDto` serialises it.
    let json = r#"[
        {
          "queued_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
          "order": {
            "external_reference": "grab-42",
            "sales_channel": "SALES_CHANNEL_DELIVERY",
            "store_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "lines": [
              { "menu_item_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "quantity_milli": 2000 }
            ],
            "placed_at_ms": 1700000000000
          }
        }
      ]"#;
    let pending: Vec<PendingOrderDto> = serde_json::from_str(json).expect("deserialize the batch");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].order.external_reference, "grab-42");
    assert_eq!(pending[0].order.lines[0].quantity_milli, 2000);
    assert!(
        pending[0].order.table_id.is_none(),
        "an absent optional field is None"
    );

    // And the ack serialises to the tagged shape the cloud's `StoreOutcome` parser expects.
    let outcome = StoreOutcome::Rejected {
        status: "invalid_argument".to_owned(),
        message: "no such item".to_owned(),
    };
    let value = serde_json::to_value(&outcome).expect("serialize");
    assert_eq!(value["outcome"], "rejected");
    assert_eq!(value["status"], "invalid_argument");
}
