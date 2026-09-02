// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `link-nats` against a live NATS server that **enforces a token**.
//!
//! Every other test in this crate runs against a broker with no authorization at all, which is how
//! a real defect survived: `async-nats` reads credentials from its connect options and never from
//! the server address, so a token written into the URL — the form `bootstrap.sh` and the new-store
//! wizard both document — was silently discarded, and the generated `nats.conf`, which does carry an
//! `authorization { token: … }` block, would have refused every connection.
//!
//! These are the cases that would have caught it. The first proves the token in the URL now
//! authenticates; the second proves the broker really is enforcing, so a green first case cannot be
//! a broker that would have accepted anything.
//!
//! Gated behind the `integration` feature, off by default. Run it with a token-enforcing server:
//!
//! ```text
//! NATS_AUTH_URL=nats://:s3cr3t@127.0.0.1:4223 \
//!   cargo test -p link-nats --features integration --test auth
//! ```

#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test scaffolding: an unreachable broker is an unrecoverable test-setup fault"
)]

use core::future::Future;

use link_nats::{NatsConfig, NatsLink};
use pos_ports::message_link::MessageLink;
use pos_proto::ids::StoreId;
use pos_proto::protocol::{Hello, MIN_SUPPORTED_PROTOCOL_VERSION};
use pos_proto::{PROTOCOL_VERSION, ReleaseTag, Ulid};

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build a multi-thread tokio runtime")
        .block_on(future)
}

/// The full URL, credentials included, exactly as an operator would write it.
fn authenticated_url() -> String {
    std::env::var("NATS_AUTH_URL").unwrap_or_else(|_| "nats://:s3cr3t@127.0.0.1:4223".to_owned())
}

/// The same broker with the credentials removed — the address alone.
fn url_without_credentials() -> String {
    let url = authenticated_url();
    let (scheme, rest) = url.split_once("://").expect("a scheme in NATS_AUTH_URL");
    let host = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
    format!("{scheme}://{host}")
}

fn hello() -> Hello {
    Hello {
        protocol_version_min: MIN_SUPPORTED_PROTOCOL_VERSION,
        protocol_version_max: PROTOCOL_VERSION,
        product_version: ReleaseTag::new("v0.1.0"),
        store_id: StoreId::new(Ulid::from_u128(0x0A07)),
        lease_token: None,
    }
}

fn config(stream: &str) -> NatsConfig {
    NatsConfig {
        stream: stream.to_owned(),
        subject: format!("{stream}.events"),
        max_messages: 8,
        max_bytes: -1,
    }
}

#[test]
fn a_token_in_the_url_authenticates_against_an_enforcing_broker() {
    // The regression test for the defect: before `endpoint::split` lifted the token out of the URL
    // and into the connect options, this connection was refused with an authorization violation.
    block_on(async {
        let link = NatsLink::connect(&authenticated_url(), config("POS_AUTH_OK"))
            .await
            .expect("the token in the URL must authenticate");
        // Reach past the TCP connection: the handshake creates the stream, so it proves the
        // authenticated session can actually do JetStream work, not merely open a socket.
        link.handshake(&hello())
            .await
            .expect("an authenticated session completes the handshake");
    });
}

#[test]
fn the_same_broker_refuses_the_address_without_the_token() {
    // Without this, the case above would also pass against a broker enforcing nothing — which is
    // precisely the state the rest of this crate's integration tests run in.
    block_on(async {
        let refused =
            NatsLink::connect(&url_without_credentials(), config("POS_AUTH_DENIED")).await;
        assert!(
            refused.is_err(),
            "a broker with an authorization block must refuse an unauthenticated connection; if \
             this passes, the server under test is not enforcing and the companion case proves \
             nothing"
        );
    });
}
