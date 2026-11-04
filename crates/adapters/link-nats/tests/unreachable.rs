// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A broker that is not up yet does not stop the link being built.
//!
//! A store PC routinely boots before its router does — after a power cut, both come back and the PC
//! is faster. The link used to fail its connect in that window, and the edge, holding no link, started
//! no publisher: the store traded on and shipped nothing until somebody restarted it. Now the client
//! connects in the background, and every call through the link fails as "unavailable" until it does,
//! which the publisher already treats as offline-and-retry. Needs no broker, so it runs in every
//! test job.

use std::time::Duration;

use link_nats::{NatsConfig, NatsLink};

#[tokio::test]
async fn a_broker_that_is_not_up_yet_does_not_fail_the_connect() {
    // Port 1 is TCP's reserved `tcpmux`: nothing a test runner hosts answers NATS there, so this is
    // a broker that is not up.
    let connected = tokio::time::timeout(
        Duration::from_secs(5),
        NatsLink::connect(
            "nats://127.0.0.1:1",
            NatsConfig {
                stream: "POS_TEST".to_owned(),
                subject: "pos.test.events".to_owned(),
                max_messages: -1,
                max_bytes: -1,
            },
        ),
    )
    .await;
    assert!(
        matches!(connected, Ok(Ok(_))),
        "connecting to a broker that is not up yet must hand back a link that keeps trying, not fail"
    );
}

/// A store whose broker is not up keeps its poll: the wake subscribes without waiting on the broker,
/// and a wait ends at its bound with nothing, rather than holding the config loop (ADR-0195).
#[tokio::test]
async fn a_wake_on_a_broker_that_is_not_up_ends_its_wait_with_nothing() {
    let link = NatsLink::connect(
        "nats://127.0.0.1:1",
        NatsConfig {
            stream: "POS_TEST".to_owned(),
            subject: "pos.test.events".to_owned(),
            max_messages: -1,
            max_bytes: -1,
        },
    )
    .await
    .expect("a link that keeps trying");
    let store = pos_proto::ids::StoreId::new(pos_proto::Ulid::from_u128(7));
    let wake = tokio::time::timeout(
        Duration::from_secs(5),
        link_nats::NatsStoreWake::subscribe(link.client().clone(), store),
    )
    .await
    .expect("subscribing does not wait on the broker")
    .expect("a subscription queued for the connect");
    let heard = tokio::time::timeout(
        Duration::from_secs(5),
        pos_ports::store_wake::StoreWake::next_signal(&wake, Duration::from_millis(250)),
    )
    .await
    .expect("the wait is bounded");
    assert!(
        matches!(heard, Ok(None)),
        "nothing heard and never Connected while the broker is down: {heard:?}"
    );
}
