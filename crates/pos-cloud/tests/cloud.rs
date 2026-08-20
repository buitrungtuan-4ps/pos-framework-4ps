// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `pos_cloud`'s ingest and rollup spine, against the in-memory fake.
//!
//! The same `Cloud` code runs here against `pos-fakes` and, in the binary, against `store-postgres`
//! (ADR-0026) — so idempotent ingest and the rollup fold are proven without a database, and the
//! store-specific behaviour (RLS, partitioning) is proven by `store-postgres`'s own suite.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use pos_cloud::{Cloud, IngestOutcome, http};
use pos_contract_tests::fixtures;
use pos_fakes::FakeStore;
use pos_proto::BusinessDate;
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(0x0ADA))
}

/// A run of activation events, re-dated onto `business_date` (`YYYY-MM-DD`).
fn dated(
    first_seed: u32,
    count: u32,
    year: i16,
    month: u8,
    day: u8,
) -> Vec<EventEnvelope<RawPayload>> {
    let date = BusinessDate::from_ymd(year, month, day).expect("a valid date");
    let mut events = fixtures::activations(store_id(), first_seed, count);
    for event in &mut events {
        event.business_date = date;
    }
    events
}

#[tokio::test]
async fn ingest_is_idempotent_by_event_id() {
    let cloud = Cloud::new(FakeStore::new());
    let events = fixtures::activations(store_id(), 1, 4);

    let first = cloud.ingest(&events).await.expect("first ingest");
    assert_eq!(
        first,
        IngestOutcome {
            appended: 4,
            duplicates: 0
        }
    );

    // At-least-once delivery replays this batch; the cloud must store nothing and report duplicates.
    let second = cloud.ingest(&events).await.expect("replayed ingest");
    assert_eq!(
        second,
        IngestOutcome {
            appended: 0,
            duplicates: 4
        }
    );

    let total: u64 = cloud
        .daily_rollups(store_id())
        .await
        .expect("rollups")
        .iter()
        .map(|day| day.total_events)
        .sum();
    assert_eq!(total, 4, "a replay must not grow the log");
}

#[tokio::test]
async fn rollups_fold_events_by_trading_day_and_type() {
    let cloud = Cloud::new(FakeStore::new());
    cloud
        .ingest(&dated(1, 3, 2026, 3, 15))
        .await
        .expect("march ingest");
    cloud
        .ingest(&dated(100, 2, 2026, 7, 1))
        .await
        .expect("july ingest");

    let rollups = cloud.daily_rollups(store_id()).await.expect("rollups");
    assert_eq!(rollups.len(), 2, "two distinct trading days");

    let march = &rollups[0];
    assert_eq!(march.business_date, "2026-03-15");
    assert_eq!(march.total_events, 3);
    // Every activation carries the same event type, so it is the only key, counted three times.
    assert_eq!(
        march.by_type.get("device.activation.completed"),
        Some(&3),
        "counts are folded per event type"
    );

    let july = &rollups[1];
    assert_eq!(july.business_date, "2026-07-01");
    assert_eq!(july.total_events, 2);
}

#[tokio::test]
async fn the_ingest_endpoint_accepts_a_batch_and_health_answers() {
    let events = fixtures::activations(store_id(), 1, 5);
    let body = serde_json::to_vec(&events).expect("serialise the batch");

    let response = http::router(Cloud::new(FakeStore::new()))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/ingest")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .expect("build the request"),
        )
        .await
        .expect("route the request");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read the body")
        .to_bytes();
    let outcome: IngestOutcome = serde_json::from_slice(&bytes).expect("parse the outcome");
    assert_eq!(
        outcome,
        IngestOutcome {
            appended: 5,
            duplicates: 0
        }
    );

    let health = http::router(Cloud::new(FakeStore::new()))
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("build the request"),
        )
        .await
        .expect("route health");
    assert_eq!(health.status(), StatusCode::OK);
}
