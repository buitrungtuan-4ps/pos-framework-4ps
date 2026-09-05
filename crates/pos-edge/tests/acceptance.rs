// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The end-to-end acceptance suite (roadmap v3 **Q1**, the v1.0 gate).
//!
//! # What makes this different from the other suites
//!
//! Every other HTTP test in this crate builds its router by hand —
//! `http::domain_router(edge, queue, pairing, sessions)` — and so proves that *the routes* work. None of
//! them proves that [`serve`](pos_edge::serve) **mounts** them. That distinction is the single most
//! expensive one in this tree's history: roadmap v3 records seven slices whose code was written,
//! unit-tested and unreachable from the running binary, and an eighth was found while scoping R5.
//! Delete a `.route(...)` line from the server's assembly today and `tests/domain_flow.rs` still
//! passes; every store stops selling.
//!
//! So this suite drives [`compose`](pos_edge::compose) — the exact router the shipped binary serves,
//! minus the socket — and walks the two flows a store actually runs:
//!
//! - **Dine-in**: pair a device over `/api/pair`, sign a person in with a PIN, seat a table, order,
//!   fire to the kitchen, bump the ticket, open the bill, settle it, cycle the table to clean.
//! - **Takeaway**: an order arrives from the cloud through [`EdgeOrderIn`] — which is how takeaway
//!   really enters a store, by relay pull rather than an inbound route — and the till then settles it
//!   over the same composed surface, proving intake and the counter share one store.
//!
//! Every request carries a token this suite obtained from the real pairing exchange, and the
//! negative cases assert the gates are actually *on* the composed router rather than merely
//! implemented somewhere.
//!
//! The store is `pos-fakes`, in memory, with no `cloud_url` — which is not a shortcut but the
//! acceptance condition itself: a shop with its cable unplugged keeps trading
//! ([ADR-0001](../../../docs/adr/0001-offline-first-store-autonomy.md)).

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pos_core::permission::PermissionSet;
use pos_edge::{
    Edge, EdgeOrderIn, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, StaffAuth, StaffRoster,
    StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_ports::order_in::{ExternalReference, InboundOrder, InboundOrderLine, OrderIn};
use pos_proto::ClockSource;
use pos_proto::ids::{MenuItemId, StationId, StoreId, TableId};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{Open, SalesChannel};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The badge code and PIN seeded into the store's roster.
const STAFF_CODE: &str = "C01";
const STAFF_PIN: &str = "2468";

/// The one item on this store's menu, priced 150,000 VND at 10% tax — so a settled bill is 165,000.
const ITEM: u128 = 500;
const UNIT_PRICE: i64 = 150_000;
const WITH_TAX: i64 = 165_000;

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(7))
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn ten_percent() -> Ratio {
    Ratio::basis_points(1_000).expect("10% is a valid rate")
}

/// A real Argon2id PHC hash of `pin`, with a fixed salt so the suite needs no RNG — the recipe the
/// offline-auth tests use.
fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::{PasswordHasher, SaltString};
    let salt = SaltString::encode_b64(b"fixed-test-salt!").expect("salt");
    Argon2::default()
        .hash_password(pin.as_bytes(), &salt)
        .expect("hash")
        .to_string()
}

/// The store's published menu, so a relayed takeaway order is priced from what the store knows
/// rather than from what the caller claimed (ADR-0063).
fn catalog() -> MenuCatalog {
    MenuCatalog::new().with(MenuEntry::new(
        MenuItemId::new(Ulid::from_u128(ITEM)),
        DisplayName::new("Margherita"),
        vnd(UNIT_PRICE),
        EdgeSession::standard_tax_class(),
    ))
}

/// A tax table charging 10% on the standard class, so the totals below are the store's own figures.
fn taxes() -> TaxRateTable {
    // A rate is keyed by (class, channel) — `docs/pos-spec.md` §5 keys tax on the sales channel — so
    // both flows below are taxed, and at the same 10%.
    let class = EdgeSession::standard_tax_class();
    TaxRateTable::new()
        .with(class, SalesChannel::DineIn, TaxRate::from_percent(10))
        .with(class, SalesChannel::Takeaway, TaxRate::from_percent(10))
}

/// The composed edge, exactly as the shipped binary assembles it, plus a device token from the real
/// pairing exchange and a person signed in on it.
///
/// The `_shutdown` guard is returned so the channel outlives the router: `compose` hands its
/// `Receiver` to whatever loops it spawns, and dropping the sender here would signal shutdown to a
/// composition that is still under test.
struct Store {
    app: Router,
    edge: Arc<Edge<FakeStore>>,
    /// The queue-number authority the composed router was given, so a test's `EdgeOrderIn` can be
    /// built over the **same** one. That is what production does — `compose` shares a single
    /// authority between intake and the counter's read route (ADR-0093) — and a test with two would
    /// prove the opposite of what it looks like it proves: the list would show no number for an
    /// order that plainly has one.
    queue: Arc<InMemoryQueueNumbers>,
    token: String,
    _shutdown: tokio::sync::watch::Sender<bool>,
}

/// Composes an edge the way `serve` does, then pairs a device and signs a person in over HTTP.
///
/// The store `bootstrap()` describes: every capability the reference preset turns on, including
/// tips.
async fn a_store() -> Store {
    a_store_where(|session| session).await
}

/// The same store with tips turned off, for the half of the tip contract that matters more: a till
/// told there is no tip to ask for.
async fn a_store_without_tips() -> Store {
    a_store_where(|session| {
        // Rebuilt from `NONE` rather than by clearing one bit, because `CapabilityContext` is a
        // bitset with no remove — and naming what stays is the honest way to say "everything except
        // tips" anyway.
        let mut capabilities = pos_core::capability::CapabilityContext::NONE;
        for capability in pos_core::capability::Capability::ALL
            .iter()
            .copied()
            .filter(|capability| *capability != pos_core::capability::Capability::Tips)
        {
            capabilities.insert(capability);
        }
        session.with_capabilities(capabilities)
    })
    .await
}

/// Composes a store, letting the caller adjust the session before it is applied.
async fn a_store_where(adjust: impl FnOnce(EdgeSession) -> EdgeSession) -> Store {
    let mut roster = StaffRoster::new();
    roster.insert(
        STAFF_CODE,
        StaffAuth {
            employee_id: Some(pos_proto::ids::EmployeeId::new(Ulid::from_u128(11))),
            permissions: PermissionSet::default(),
            pin_phc: Some(hash_of(STAFF_PIN)),
        },
    );
    let session = adjust(
        EdgeSession::bootstrap()
            .with_staff(roster)
            .with_menu(catalog())
            .with_tax_rates(taxes()),
    );
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(store_id()),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("the edge seeds"),
    );

    let config = pos_edge::EdgeConfig {
        // Port 0 is never bound here — `compose` does not touch the socket, which is the point.
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        store_id: store_id(),
        advertised_ip: None,
        // LAN-only on purpose: no cloud loops, no network, and the shop still trades (ADR-0001).
        cloud_url: None,
        store_path: "unused-in-memory.sqlite".into(),
        nats: None,
        sign_in_idle_timeout_minutes: 30,
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let queue = Arc::new(InMemoryQueueNumbers::new());
    let composed = pos_edge::compose(config, Arc::clone(&edge), Arc::clone(&queue), &shutdown_rx)
        .await
        .expect("the edge composes");

    // Pair a device the way a device does: mint a code on the box, then redeem it over the route the
    // operator UI posts to. Nothing here reaches inside `Pairing` to fabricate a token.
    let code = composed
        .pairing
        .mint(pos_edge::SystemClock.now())
        .expect("the OS entropy source mints a pairing code")
        .as_str()
        .to_owned();

    let (status, paired) = post(
        composed.app.clone(),
        None,
        "/api/pair",
        Some(json!({ "code": code })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a fresh code pairs a device");
    let token = paired["device_token"]
        .as_str()
        .expect("the pairing response carries a device token")
        .to_owned();

    let (status, _) = post(
        composed.app.clone(),
        Some(&token),
        "/api/session/sign-in",
        Some(json!({ "code": STAFF_CODE, "pin": STAFF_PIN })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the seeded staff signs in");

    Store {
        app: composed.app,
        edge,
        queue,
        token,
        _shutdown: shutdown_tx,
    }
}

/// Sends a request through the composed router. `token` absent means no `Authorization` header at
/// all — which is how the negative cases below exercise the gates.
async fn send(
    app: Router,
    token: Option<&str>,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let body = body.map_or_else(Body::empty, |value| Body::from(value.to_string()));
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .oneshot(request.body(body).expect("request builds"))
        .await
        .expect("the composed router responds");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn post(
    app: Router,
    token: Option<&str>,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    send(app, token, "POST", uri, body).await
}

fn a_line_body() -> Value {
    json!({
        "menu_item_id": MenuItemId::new(Ulid::from_u128(ITEM)),
        "display_name": "Margherita",
        "quantity": Quantity::ONE,
        "unit_price": vnd(UNIT_PRICE),
        "line_total": vnd(UNIT_PRICE),
        "tax_class_id": EdgeSession::standard_tax_class(),
        "tax_rate": ten_percent(),
        "note_present": false,
    })
}

// ---------------------------------------------------------------------------
// The acceptance flows.
// ---------------------------------------------------------------------------

/// Seats a table, orders one item, fires it to a station and lets the kitchen bump the ticket — the
/// beats before any money changes hands.
///
/// Every step goes through the composed router, so this helper is itself part of what the acceptance
/// test asserts: if `serve` stops mounting the floor, order or KDS routes, this fails.
async fn a_fired_and_prepared_order(store: &Store, table: TableId) {
    // Seat.
    let (status, view) = post(
        store.app.clone(),
        Some(&store.token),
        &format!("/api/tables/{table}/seat"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["state"], "TABLE_STATE_OCCUPIED");

    // Order.
    let (status, line) = post(
        store.app.clone(),
        Some(&store.token),
        &format!("/api/tables/{table}/lines"),
        Some(a_line_body()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let line_id = line["order_line_id"]
        .as_str()
        .expect("a line id")
        .to_owned();
    let order_id = line["order_id"].as_str().expect("an order id").to_owned();

    // Fire to the kitchen. The station is named again at the bump, so the kitchen display is
    // acknowledging the ticket it was actually sent.
    let station = StationId::new(Ulid::from_u128(9));
    let (status, fired) = post(
        store.app.clone(),
        Some(&store.token),
        &format!("/api/lines/{line_id}/fire"),
        Some(json!({ "station_id": station })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fired["state"], "ORDER_LINE_STATE_FIRED");

    // The kitchen bumps the ticket. This is the beat that was durable-but-unwired once already
    // (P6 residuals), so the acceptance flow walks through it rather than around it.
    let (status, bumped) = post(
        store.app.clone(),
        Some(&store.token),
        "/api/kds/bump",
        Some(json!({
            "order_id": order_id,
            "station_id": station,
            "order_line_ids": [line_id],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the KDS bump route is mounted");
    assert_eq!(
        bumped["order_line_ids"],
        json!([line_id]),
        "the bump reports back the line it marked prepared"
    );
}

/// The health probe answers unauthenticated on the composed router: it is what a service manager
/// watches, so it must not sit behind the device gate.
#[tokio::test]
async fn the_health_probe_answers_unauthenticated_on_the_composed_edge() {
    let store = a_store().await;
    let (status, health) = send(store.app.clone(), None, "GET", "/healthz", None).await;
    assert_eq!(status, StatusCode::OK, "/healthz is mounted and open");
    assert_eq!(
        health["protocol_version"],
        pos_proto::PROTOCOL_VERSION,
        "the probe reports the wire version this binary speaks"
    );
}

/// Dine-in, whole, over the router `serve` builds: pair → sign in → seat → order → fire → bump →
/// bill → settle → clean.
///
/// Each assertion is a beat an operator would notice going wrong, and every one of them runs against
/// the *composed* surface — so a route the server forgets to mount fails here.
#[tokio::test]
async fn a_table_is_sold_end_to_end_on_the_composed_edge() {
    let store = a_store().await;
    let table = TableId::new(Ulid::from_u128(700));
    a_fired_and_prepared_order(&store, table).await;

    // What the table owes, assembled by the edge — the till settles against the edge's figure, never
    // one it computed itself (roadmap-v3 E5).
    let (status, check) = send(
        store.app.clone(),
        Some(&store.token),
        "GET",
        &format!("/api/tables/{table}/check"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        check["subtotal"]["amount_minor"], UNIT_PRICE,
        "the edge quotes the line ex-tax"
    );
    assert_eq!(
        check["total_due"]["amount_minor"], WITH_TAX,
        "and 150,000 plus 10% tax is what the till will settle against"
    );

    // Open the bill.
    let (status, bill) = post(
        store.app.clone(),
        Some(&store.token),
        &format!("/api/tables/{table}/bill"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bill["table_state"], "TABLE_STATE_AWAITING_PAYMENT");
    let bill_id = bill["bill_id"].as_str().expect("a bill id").to_owned();

    // Settle in cash: a gapless receipt, and the table wants cleaning.
    let (status, settled) = post(
        store.app.clone(),
        Some(&store.token),
        &format!("/api/bills/{bill_id}/settle"),
        Some(json!({
            "payments": [{
                "method": "PAYMENT_METHOD_CASH",
                "tendered": vnd(WITH_TAX),
                "applied_to_bill": vnd(WITH_TAX),
            }],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settled["state"], "BILL_STATE_SETTLED");
    assert_eq!(settled["receipt_number"], 1, "receipts start at one");

    // Clean down: the table is sellable again, which is what closes the loop.
    let (status, cleaned) = post(
        store.app.clone(),
        Some(&store.token),
        &format!("/api/tables/{table}/clean"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cleaned["state"], "TABLE_STATE_FREE");
}

/// Takeaway: an order relayed from the cloud is accepted, priced by the store, given a queue number,
/// and idempotent under retry.
///
/// Takeaway does not arrive by an inbound edge route — the store pulls it
/// ([ADR-0058](../../../docs/adr/0058-cloud-store-relay.md)) — so intake runs through the port, as it
/// does in the field.
///
/// This test covers **intake**: accepted, priced, queued, idempotent. Taking the money is the next
/// test down, [`a_relayed_takeaway_order_is_charged_at_the_counter`], and the split is deliberate —
/// intake and payment failed independently while the bill was table-keyed.
///
/// # What this test used to record, and how it was closed
///
/// It used to end here and say so, because **the edge had no way to settle a takeaway order**.
/// `Edge::open_bill` was the only path to a bill and it took a `TableId`: it gated on that table
/// being `Occupied` and resolved the order through `order_for_table`. A takeaway order is tableless
/// by design (roadmap-v3 PR-1b), so `order_for_table` could never find it, no route opened a bill
/// without a table, and every takeaway order a store accepted was priced, queued, fired — and could
/// not be charged for.
///
/// Writing this suite is what found that, and it was filed as its own slice rather than papered over
/// here, because closing it meant keying the bill on the order — a domain change, not a test
/// fixture. That decision is [ADR-0093](../../../docs/adr/0093-bill-keyed-on-order.md) and it has
/// landed, so the flow it blocked is now asserted rather than described.
#[tokio::test]
async fn a_relayed_takeaway_order_is_accepted_and_priced_by_the_store() {
    let store = a_store().await;
    let intake = EdgeOrderIn::new(
        Arc::clone(&store.edge),
        Arc::clone(&store.queue),
        pos_edge::system_device_id(store_id()),
    );

    // The cloud relays one Margherita, quoting a price deliberately below the store's, so the
    // repricing rule is exercised rather than assumed: a marketplace's stale cache is not authority
    // over what a shop charges.
    let order = |reference: &str, quote: i64| InboundOrder {
        external_reference: ExternalReference::parse(reference).expect("a bounded reference"),
        sales_channel: Open::from_known(SalesChannel::Takeaway),
        store_id: store_id(),
        table_id: None,
        subject_id: None,
        lines: vec![InboundOrderLine {
            menu_item_id: MenuItemId::new(Ulid::from_u128(ITEM)),
            quantity: Quantity::ONE,
            modifier_menu_item_ids: Vec::new(),
            quoted_unit_price: Some(vnd(quote)),
            note: None,
        }],
        placed_at: pos_edge::SystemClock.now(),
    };

    let accepted = intake
        .submit(&order("ORDER-1", UNIT_PRICE - 50_000))
        .await
        .expect("the store accepts a relayed order with no cloud reachable");
    assert!(accepted.created, "a first submit creates the order");
    assert!(
        accepted.repriced,
        "the store repriced the caller's stale quote rather than honouring it"
    );
    assert_eq!(
        accepted.total,
        vnd(UNIT_PRICE),
        "the total is the store's own figure, not the caller's quote — ex-tax, as the priced lines \
         are; tax joins at the bill"
    );
    assert_eq!(
        accepted.queue_number,
        Some(1),
        "a counter order gets a queue number, and it starts at one"
    );

    // A retry of the same reference is the same order, not a second one — the property that stops a
    // flaky relay double-charging a customer, and the reason the intake ledger is transactional
    // (ADR-0064).
    let retried = intake
        .submit(&order("ORDER-1", UNIT_PRICE - 50_000))
        .await
        .expect("a retry is answered rather than refused");
    assert!(!retried.created, "the retry created no second order");
    assert_eq!(
        retried.order_id, accepted.order_id,
        "the retry resolves to the order the first call made"
    );
    assert_eq!(
        retried.queue_number, accepted.queue_number,
        "and it does not burn a second queue number"
    );

    // The same reference on another channel is a different order: the idempotency key is scoped by
    // channel, so a marketplace and the counter can both use "ORDER-1" without colliding.
    let elsewhere = intake
        .submit(&InboundOrder {
            sales_channel: Open::from_known(SalesChannel::DineIn),
            table_id: Some(TableId::new(Ulid::from_u128(704))),
            ..order("ORDER-1", UNIT_PRICE)
        })
        .await
        .expect("the same reference on another channel is a new order");
    assert!(elsewhere.created);
    assert_ne!(
        elsewhere.order_id, accepted.order_id,
        "one reference on two channels is two orders"
    );
}

/// Takeaway, end to end: a relayed order is **paid for at the counter**, through the routes a device
/// calls ([ADR-0093](../../../docs/adr/0093-bill-keyed-on-order.md)).
///
/// This is the flow the suite could not assert before the bill was keyed on the order, and it is
/// driven over HTTP rather than through `Edge` directly on purpose: the domain change would be worth
/// nothing to a store if the counter had no route to reach it, which is the failure mode this
/// roadmap keeps catching. So the order arrives through the port, as it does in the field, and every
/// step after that is a request a till makes.
///
/// No table is created, seated or named anywhere in this test. That is the assertion: a store that
/// has never opened a table can still take money.
#[expect(
    clippy::too_many_lines,
    reason = "one operator flow, read top to bottom: find the order on the counter list, read the \
              check, open its bill, settle in cash, see it leave the list, and be refused a second \
              bill. Splitting it into helpers would hide the order of the steps, which is the \
              property an acceptance test exists to pin."
)]
#[tokio::test]
async fn a_relayed_takeaway_order_is_charged_at_the_counter() {
    let store = a_store().await;
    let intake = EdgeOrderIn::new(
        Arc::clone(&store.edge),
        Arc::clone(&store.queue),
        pos_edge::system_device_id(store_id()),
    );

    let accepted = intake
        .submit(&InboundOrder {
            external_reference: ExternalReference::parse("COUNTER-1").expect("a reference"),
            sales_channel: Open::from_known(SalesChannel::Takeaway),
            store_id: store_id(),
            table_id: None,
            subject_id: None,
            lines: vec![InboundOrderLine {
                menu_item_id: MenuItemId::new(Ulid::from_u128(ITEM)),
                quantity: Quantity::ONE,
                modifier_menu_item_ids: Vec::new(),
                quoted_unit_price: None,
                note: None,
            }],
            placed_at: pos_edge::SystemClock.now(),
        })
        .await
        .expect("the store accepts a relayed counter order");

    // The cashier finds the order the only way they can: the counter's list. A relayed order is on
    // no floor plan, so without this route they would have to be *told* the ULID — which is the
    // one thing an operator can never be expected to type.
    let (status, listed) = send(
        store.app.clone(),
        Some(&store.token),
        "GET",
        "/api/orders/open",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the counter list route is mounted");
    let orders = listed.as_array().expect("a list of orders");
    assert_eq!(orders.len(), 1, "the relayed order is waiting: {listed}");
    let waiting = &orders[0];
    assert_eq!(
        waiting["queue_number"], 1,
        "showing the number the counter shouted, read from the same authority intake allocated \
         from — not a fresh one this route minted"
    );
    assert_eq!(
        waiting["total_due"]["amount_minor"], WITH_TAX,
        "and what is owed, with tax"
    );
    assert_eq!(
        waiting["items"][0]["display_name"], "Margherita",
        "and what it is, so a cashier recognises it: {waiting}"
    );
    assert!(
        waiting["bill_id"].is_null(),
        "no bill open on it yet, so the screen opens one rather than resuming"
    );
    let order_id = waiting["order_id"]
        .as_str()
        .expect("the list names the order to bill")
        .to_owned();
    assert_eq!(
        order_id,
        accepted.order_id.to_string(),
        "and it is the order intake accepted"
    );

    // The cashier reads what is owed before taking money, from the order-keyed check — the
    // table-keyed one cannot answer for an order that sits on no table.
    let (status, check) = send(
        store.app.clone(),
        Some(&store.token),
        "GET",
        &format!("/api/orders/{order_id}/check"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the counter check route is mounted");
    assert_eq!(
        check["total_due"]["amount_minor"], WITH_TAX,
        "the till shows the store's own figure with tax, not the ex-tax intake total"
    );

    // Open the bill on the order. Nothing here mentions a table.
    let (status, bill) = post(
        store.app.clone(),
        Some(&store.token),
        &format!("/api/orders/{order_id}/bill"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a tableless order takes a bill");
    assert_eq!(bill["state"], "BILL_STATE_OPEN");
    assert!(
        bill["table_state"].is_null(),
        "no table state is reported, because there is no table: {bill}"
    );
    let bill_id = bill["bill_id"].as_str().expect("a bill id").to_owned();

    // And settle it in cash, exactly. The receipt number comes off the same gapless per-store
    // counter a dine-in sale draws on (ADR-0025) — a counter sale is not a second ledger.
    let (status, settled) = post(
        store.app.clone(),
        Some(&store.token),
        &format!("/api/bills/{bill_id}/settle"),
        Some(json!({
            "payments": [{
                "method": "PAYMENT_METHOD_CASH",
                "tendered": vnd(WITH_TAX),
                "applied_to_bill": vnd(WITH_TAX),
            }]
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the counter sale settles: {settled}"
    );
    assert_eq!(settled["state"], "BILL_STATE_SETTLED");
    assert_eq!(settled["receipt_number"], 1);
    assert_eq!(settled["total_due"]["amount_minor"], WITH_TAX);
    assert!(
        settled["table_state"].is_null(),
        "and cycles no table on the way out"
    );

    // Settled, it leaves the counter list — otherwise every paid order of the day would pile up in
    // front of the next customer's.
    let (status, after) = send(
        store.app.clone(),
        Some(&store.token),
        "GET",
        "/api/orders/open",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        after.as_array().map(Vec::len),
        Some(0),
        "a paid order is off the counter list: {after}"
    );

    // A second bill on the same order is refused. On the floor the table state machine used to do
    // this by accident; on the counter there is no table, so the order index is the only thing
    // between one receipt and two for one meal.
    let (status, refused) = post(
        store.app.clone(),
        Some(&store.token),
        &format!("/api/orders/{order_id}/bill"),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "one order, one bill: {refused}"
    );
}

/// The cash shift, over the composed router: open with a float, count blind, close with the variance.
#[tokio::test]
async fn a_cash_shift_opens_counts_and_closes_on_the_composed_edge() {
    let store = a_store().await;

    let (status, opened) = post(
        store.app.clone(),
        Some(&store.token),
        "/api/shifts",
        Some(json!({ "opening_float": vnd(500_000) })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the shift route is mounted");
    let shift_id = opened["shift_id"].as_str().expect("a shift id").to_owned();

    // A blind count: the operator declares a figure without being shown the expected one.
    let (status, _) = post(
        store.app.clone(),
        Some(&store.token),
        &format!("/api/shifts/{shift_id}/count"),
        Some(json!({ "counted_minor": 500_000 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, closed) = post(
        store.app.clone(),
        Some(&store.token),
        &format!("/api/shifts/{shift_id}/close"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(closed["state"], "SHIFT_STATE_CLOSED");
}

// ---------------------------------------------------------------------------
// The gates are on the composed router, not merely implemented.
// ---------------------------------------------------------------------------

/// An unpaired host on the store LAN gets nothing — not the domain routes, and not the event stream.
///
/// This is the assertion that would have failed before S0c, when `/ws` streamed every committed
/// order, bill and settlement to anything that could route to the box. It belongs in the acceptance
/// suite rather than only in `tests/ws.rs` because the question it answers is about the *server*: is
/// the gate mounted where the binary serves it?
#[tokio::test]
async fn an_unpaired_caller_reaches_no_domain_route_on_the_composed_edge() {
    let store = a_store().await;
    let table = TableId::new(Ulid::from_u128(702));

    for (method, uri) in [
        ("GET", format!("/api/tables/{table}")),
        ("POST", format!("/api/tables/{table}/seat")),
        ("GET", "/api/menu".to_owned()),
        ("GET", "/api/floor".to_owned()),
        ("GET", "/api/session".to_owned()),
        ("POST", "/api/shifts".to_owned()),
        ("GET", "/api/pair/devices".to_owned()),
    ] {
        let (status, _) = send(store.app.clone(), None, method, &uri, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {uri} must refuse an unpaired caller"
        );
    }
}

/// The till learns the two published facts it has to obey: whether the store takes tips, and which
/// tender it accepts.
///
/// Before this, both were live in `EdgeSession` and read by nobody. The consequences were not
/// symmetric. `accepted_tender` made the till offer a method the edge would refuse, so a cash-only
/// store's refusal landed as a `400` in front of the guest. Tips were worse than a bad refusal:
/// with no entry field anywhere in the till, `tip_amount` was zero on **every** payment a real
/// store took, whatever the capability said — the domain half shipped and the operator half did
/// not.
///
/// Read through the composed router rather than by calling the handler, because the question is
/// whether the shipped binary serves them.
#[tokio::test]
async fn the_till_is_told_whether_the_store_takes_tips_and_what_tender_it_accepts() {
    let store = a_store().await;
    let (status, menu) = send(
        store.app.clone(),
        Some(&store.token),
        "GET",
        "/api/menu",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // `bootstrap()` turns Tips on (pos-core `capability.rs`), so this store takes them and the till
    // is told so. The false case is the one below.
    assert_eq!(
        menu["tips_enabled"],
        Value::Bool(true),
        "the till must be told whether a tip is even allowed"
    );
    // No `tender` node published, so nothing is restricted — and `null` says exactly that, rather
    // than listing all seven methods. A store that restricts nothing keeps accepting a method added
    // to the enum later without a config change.
    assert_eq!(
        menu["accepted_tender"],
        Value::Null,
        "an unrestricted store publishes no list, and null is not an empty list"
    );
}

/// And the false case, which is the one that matters: a store with tips off says so, so the till
/// shows no tip entry instead of offering something `decide_bill` will refuse.
#[tokio::test]
async fn a_store_with_tips_off_tells_the_till_not_to_ask_for_one() {
    let store = a_store_without_tips().await;
    let (status, menu) = send(
        store.app.clone(),
        Some(&store.token),
        "GET",
        "/api/menu",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        menu["tips_enabled"],
        Value::Bool(false),
        "with the capability off the till is told there is no tip to take"
    );
}

/// A paired device with nobody signed in may sign someone in, and may do nothing else.
///
/// The split is the whole point of the two-gate design (ADR-0084): a till at the start of a shift
/// has a valid device token and no actor, and must not be able to sell under a forged one.
#[tokio::test]
async fn a_paired_device_with_no_one_signed_in_can_only_sign_in() {
    let store = a_store().await;
    let table = TableId::new(Ulid::from_u128(703));

    // Sign the person out again, leaving the device paired but empty-handed.
    let (status, _) = post(
        store.app.clone(),
        Some(&store.token),
        "/api/session/sign-out",
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the sign-out route is mounted, and has nothing to say back"
    );

    let (status, _) = post(
        store.app.clone(),
        Some(&store.token),
        &format!("/api/tables/{table}/seat"),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a paired but signed-out device is refused a command, not merely unauthorised"
    );

    // And the way back in is open.
    let (status, _) = post(
        store.app.clone(),
        Some(&store.token),
        "/api/session/sign-in",
        Some(json!({ "code": STAFF_CODE, "pin": STAFF_PIN })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the device can sign someone back in"
    );
}

/// Retiring a device is reachable on the composed router, and a retired token stops working
/// immediately (ADR-0091).
#[tokio::test]
async fn a_revoked_device_is_refused_by_the_composed_edge() {
    let store = a_store().await;

    let (status, devices) = send(
        store.app.clone(),
        Some(&store.token),
        "GET",
        "/api/pair/devices",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the devices route is mounted");
    assert_eq!(devices["devices"], 1, "one device is paired");
    // O1: the list, not just the count — `revoke` takes a device id, and this is the only surface
    // that hands one out. The caller's own row is marked so an operator does not retire the tablet
    // in their hand while looking for the one that walked out.
    let paired = devices["paired"]
        .as_array()
        .expect("the paired devices are listed");
    assert_eq!(paired.len(), 1, "the one paired device is named");
    assert_eq!(
        paired[0]["this_device"], true,
        "the requesting device is marked as itself"
    );
    assert!(
        paired[0]["device_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "the device id is what revoke takes back"
    );
    assert!(
        paired[0]["paired_at_ms"].as_i64().is_some(),
        "and when it paired, which is how an operator tells the tills apart"
    );

    // Retiring by id is the ordinary path: name the lost tablet, leave everyone else trading.
    let this_device = paired[0]["device_id"]
        .as_str()
        .expect("a device id")
        .to_owned();
    let (status, _) = post(
        store.app.clone(),
        Some(&store.token),
        "/api/pair/revoke",
        Some(json!({ "device_id": "00000000000000000000000000" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "revoking a device that is not paired is a no-op, not an error"
    );
    let (status, devices) = send(
        store.app.clone(),
        Some(&store.token),
        "GET",
        "/api/pair/devices",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "this device is untouched");
    assert_eq!(
        devices["paired"][0]["device_id"], this_device,
        "retiring a stranger did not retire the caller"
    );

    // Break glass: retire every device.
    let (status, _) = post(
        store.app.clone(),
        Some(&store.token),
        "/api/pair/revoke",
        Some(json!({})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the revoke route is mounted"
    );

    let (status, _) = send(
        store.app.clone(),
        Some(&store.token),
        "GET",
        "/api/pair/devices",
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the retired token stops working at once"
    );
}
