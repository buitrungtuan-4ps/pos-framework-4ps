// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Performance budgets for the edge's hot paths, at a large trading history.
//!
//! `#[ignore]`d, because a timing assertion in a debug build on a shared runner is a number about the
//! runner ([ADR-0109](../../../docs/adr/0109-counting-the-taps-an-operator-makes.md) makes the same
//! call for the browser gate). Run in a release build with `just bench`:
//!
//! ```text
//! cargo test -p pos-edge --release --test perf_budget -- --ignored --nocapture
//! ```
//!
//! `POS_BENCH_HISTORY` sets how many settled orders the store has behind it (default 30,000, the
//! figure the budgets are stated at) and `POS_BENCH_STORE=sqlite` runs over the real SQLite adapter
//! rather than the in-memory fakes.
//!
//! # What it measures, and why these three
//!
//! The first edge benchmark found the store getting slower with age: every settled order stayed in
//! the in-memory projection, the reads a device makes at boot scanned all of them, and they did it
//! holding the lock every sale needs (finding F2). At 30,000 historical orders `GET /api/orders/live`
//! took ~97 ms, and a colleague's add-line waited behind it at ~103 ms. So:
//!
//! - **`GET /api/orders/live`** — what every reload, wake and `resync` reads;
//! - **adding a line** on a quiet store;
//! - **adding a line while another device reads the live orders in a loop** — the contention case,
//!   which is the one an operator feels.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pos_core::permission::PermissionSet;
use pos_edge::pairing::Minter;
use pos_edge::{
    Edge, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing, Sessions, StaffAuth,
    StaffRoster, StoreIdentity, SystemClock,
};
use pos_fakes::FakeStore;
use pos_ports::SubjectStore;
use pos_ports::event_store::EventStore;
use pos_proto::ClockSource;
use pos_proto::CurrencyCode;
use pos_proto::ids::{EmployeeId, MenuItemId, StationId, StoreId, TableId};
use pos_proto::money::{Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::ulid::Ulid;
use serde_json::{Value, json};
use tower::ServiceExt;

/// The budgets, at [`DEFAULT_HISTORY`] settled orders. A release build on a 4-core store PC.
const LIVE_ORDERS_P95: Duration = Duration::from_millis(20);
const ADD_LINE_P95: Duration = Duration::from_millis(5);
const ADD_LINE_UNDER_READS_P95: Duration = Duration::from_millis(10);

const DEFAULT_HISTORY: usize = 30_000;
/// Tables open at the moment of measurement, each with a fired order on it — a busy service.
const LIVE_TABLES: u128 = 40;
const LINES_PER_LIVE_TABLE: usize = 5;
const SAMPLES: usize = 300;

const STAFF_CODE: &str = "C01";
const STAFF_PIN: &str = "2468";

fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    Argon2::default()
        .hash_password_with_salt(pin.as_bytes(), b"a-fixed-test-slt")
        .expect("hash")
        .to_string()
}

async fn app_over<S>(store: S) -> (Router, String)
where
    S: EventStore + SubjectStore + Clone + Send + Sync + 'static,
{
    let mut roster = StaffRoster::new();
    roster.insert(
        STAFF_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
            permissions: PermissionSet::default(),
            discount_ceiling: None,
            pin_phc: Some(hash_of(STAFF_PIN)),
        },
    );
    let edge = Arc::new(
        Edge::new(
            store,
            StoreIdentity::for_store(StoreId::new(Ulid::from_u128(7))),
            EdgeSession::bootstrap().with_staff(roster),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed"),
    );
    let pairing = Arc::new(Pairing::new());
    let now = SystemClock.now();
    let (code, _) = pairing.mint(now, Minter::Boot).expect("mint");
    let token = pairing
        .redeem(&code, now)
        .await
        .expect("redeem")
        .token()
        .expect("a fresh code pairs")
        .as_str()
        .to_owned();
    let app = pos_edge::http::domain_router(
        edge,
        InMemoryQueueNumbers::new(),
        Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new()),
        pos_edge::print_queue::InMemoryPrintQueue::new(),
        pos_edge::print_wake::SharedPrintWake::new(),
        pairing,
        Arc::new(Sessions::new()),
        &Arc::new(pos_edge::origins::Origins::new()),
    );
    let (status, _) = send(
        &app,
        &token,
        "POST",
        "/api/session/sign-in",
        Some(json!({ "code": STAFF_CODE, "pin": STAFF_PIN })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    (app, token)
}

async fn send(
    app: &Router,
    token: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let body = body.map_or_else(Body::empty, |value| Body::from(value.to_string()));
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(body)
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("responds");
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

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn line_body() -> Value {
    json!({
        "menu_item_id": MenuItemId::new(Ulid::from_u128(500)),
        "display_name": "Margherita",
        "quantity": Quantity::ONE,
        "unit_price": vnd(150_000),
        "line_total": vnd(150_000),
        "tax_class_id": EdgeSession::standard_tax_class(),
        "tax_rate": Ratio::basis_points(1_000).expect("a valid rate"),
        "note_present": false,
    })
}

fn station() -> Value {
    json!({ "station_id": StationId::new(Ulid::from_u128(9)) })
}

/// Seats `table`, adds `lines` lines and fires them. Returns the order id.
async fn open_and_fire(app: &Router, token: &str, table: TableId, lines: usize) -> String {
    let (status, _) = send(
        app,
        token,
        "POST",
        &format!("/api/tables/{table}/seat"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seat");
    let mut order_id = String::new();
    for _ in 0..lines {
        let (status, line) = send(
            app,
            token,
            "POST",
            &format!("/api/tables/{table}/lines"),
            Some(line_body()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "add line");
        line["order_id"]
            .as_str()
            .expect("order")
            .clone_into(&mut order_id);
    }
    let (status, _) = send(
        app,
        token,
        "POST",
        &format!("/api/orders/{order_id}/fire"),
        Some(station()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "fire");
    order_id
}

/// One full table turn that ends settled and clean: the history a store accumulates.
async fn turn(app: &Router, token: &str, table: TableId) {
    open_and_fire(app, token, table, 2).await;
    let (status, bill) = send(
        app,
        token,
        "POST",
        &format!("/api/tables/{table}/bill"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "bill");
    let bill_id = bill["bill_id"].as_str().expect("bill").to_owned();
    let (status, _) = send(
        app,
        token,
        "POST",
        &format!("/api/bills/{bill_id}/settle"),
        Some(json!({ "payments": [{
            "method": "PAYMENT_METHOD_CASH",
            "tendered": vnd(330_000),
            "applied_to_bill": vnd(330_000),
        }] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "settle");
    let (status, _) = send(
        app,
        token,
        "POST",
        &format!("/api/tables/{table}/clean"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "clean");
}

fn p(samples: &mut [Duration], quantile: usize) -> Duration {
    samples.sort_unstable();
    let index = (samples.len() * quantile / 100).min(samples.len().saturating_sub(1));
    samples.get(index).copied().unwrap_or_default()
}

/// Milliseconds with two decimals, in integers: the workspace has no floats, benchmarks included.
fn ms(duration: Duration) -> String {
    let micros = duration.as_micros();
    format!("{}.{:02}", micros / 1000, (micros % 1000) / 10)
}

/// Writes one line of the report. Not `eprintln!`, which the workspace reserves for nothing; a
/// benchmark's report is its output, and `--nocapture` shows it.
fn report_line(line: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// The store the budgets are stated at: `history` settled orders behind it and [`LIVE_TABLES`]
/// tables open now. Returns how long the history took to seed.
async fn seed(app: &Router, token: &str, history: usize) -> (Vec<TableId>, Duration) {
    let seeded = tokio::time::Instant::now();
    let history_table = TableId::new(Ulid::from_u128(999));
    for _ in 0..history {
        turn(app, token, history_table).await;
    }
    let seed_time = seeded.elapsed();
    let live: Vec<TableId> = (0..LIVE_TABLES)
        .map(|n| TableId::new(Ulid::from_u128(1_000 + n)))
        .collect();
    for table in &live {
        open_and_fire(app, token, *table, LINES_PER_LIVE_TABLE).await;
    }
    (live, seed_time)
}

/// Adds [`SAMPLES`] lines to `table`, `gap` apart, and returns how long each took.
async fn add_lines(app: &Router, token: &str, table: TableId, gap: Duration) -> Vec<Duration> {
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        if !gap.is_zero() {
            tokio::time::sleep(gap).await;
        }
        let started = tokio::time::Instant::now();
        let (status, _) = send(
            app,
            token,
            "POST",
            &format!("/api/tables/{table}/lines"),
            Some(line_body()),
        )
        .await;
        samples.push(started.elapsed());
        assert_eq!(status, StatusCode::OK);
    }
    samples
}

async fn measure<S>(store: S, history: usize)
where
    S: EventStore + SubjectStore + Clone + Send + Sync + 'static,
{
    let (app, token) = app_over(store).await;
    let (live, seed_time) = seed(&app, &token, history).await;

    let mut live_reads = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = tokio::time::Instant::now();
        let (status, body) = send(&app, &token, "GET", "/api/orders/live", None).await;
        live_reads.push(started.elapsed());
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body.as_array().map(Vec::len),
            Some(live.len()),
            "only the live orders"
        );
    }

    let mut adds = add_lines(&app, &token, live[0], Duration::ZERO).await;

    // Another device reloading in a loop while this one sells. The till taps at a human pace, so
    // the adds are spaced out: 300 adds back to back take less time than one slow read, and would
    // measure nothing but luck.
    let (reading, mut first_read) = tokio::sync::watch::channel(false);
    let reader = {
        let (app, token) = (app.clone(), token.clone());
        tokio::spawn(async move {
            loop {
                let _ = send(&app, &token, "GET", "/api/orders/live", None).await;
                let _ = reading.send(true);
                tokio::task::yield_now().await;
            }
        })
    };
    let _ = first_read.wait_for(|read| *read).await;
    let mut contended = add_lines(&app, &token, live[1], Duration::from_millis(3)).await;
    reader.abort();

    let report = [
        (
            "GET /api/orders/live",
            p(&mut live_reads, 50),
            p(&mut live_reads, 95),
            LIVE_ORDERS_P95,
        ),
        (
            "add a line",
            p(&mut adds, 50),
            p(&mut adds, 95),
            ADD_LINE_P95,
        ),
        (
            "add a line, another device reading",
            p(&mut contended, 50),
            p(&mut contended, 95),
            ADD_LINE_UNDER_READS_P95,
        ),
    ];
    report_line(&format!(
        "history: {history} settled orders (seeded in {} ms), {LIVE_TABLES} live tables",
        seed_time.as_millis()
    ));
    for (name, p50, p95, budget) in &report {
        report_line(&format!(
            "{name:<38} p50 {:>8} ms   p95 {:>8} ms   budget {:>5} ms   {}",
            ms(*p50),
            ms(*p95),
            budget.as_millis(),
            if p95 <= budget { "ok" } else { "OVER" }
        ));
    }
    for (name, _, p95, budget) in report {
        assert!(
            p95 <= budget,
            "{name}: p95 {p95:?} is over its budget of {budget:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "a benchmark: run in a release build with `just bench`"]
async fn the_hot_paths_stay_inside_their_budgets_at_a_large_history() {
    let history = std::env::var("POS_BENCH_HISTORY")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_HISTORY);
    if std::env::var("POS_BENCH_STORE").is_ok_and(|value| value == "sqlite") {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = store_sqlite::SqliteStore::open(dir.path().join("store.db")).expect("open");
        measure(store, history).await;
    } else {
        measure(FakeStore::default(), history).await;
    }
}
