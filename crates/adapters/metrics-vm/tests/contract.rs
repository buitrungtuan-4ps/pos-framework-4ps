// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `metrics-vm` against the shared `MetricsSink` contract, plus the `VictoriaMetrics` import wire.
//!
//! The contract runs in process against a capturing transport, because the port's contract is this
//! adapter's queueing and back-pressure — not `VictoriaMetrics`' storage
//! ([ADR-0031](../../../docs/adr/0031-cloud-adapter-transports.md)). A second test drives the real
//! [`HttpTransport`](metrics_vm::HttpTransport) against an in-process TCP mock and asserts the
//! exact bytes it emits, so the wire format is pinned without a live `VictoriaMetrics`.

// Test scaffolding: the module-level harness and mock helpers are outside the `#[test]`/`#[cfg(test)]`
// scope that `allow-expect-in-tests` covers, so the unrecoverable-setup panics and the byte-slicing
// in the HTTP mock are allowed here explicitly.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test scaffolding: a failed runtime, socket, or fixture is an unrecoverable test-setup \
              fault, and the in-process HTTP mock parses a small request by slicing"
)]

use std::future::Future;
use std::sync::{Arc, Mutex};

use metrics_vm::{MetricTransport, QUEUE_CAPACITY, VmMetrics};
use pos_contract_tests::fixtures;
use pos_contract_tests::harness::{HarnessError, MetricsSinkHarness, Setup};
use pos_ports::PortError;
use pos_ports::metrics_sink::{
    MetricLabel, MetricLabelValue, MetricName, MetricSample, MetricUnit, MetricsSink,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Drives a future on a fresh multi-thread runtime with IO enabled — the flush task and the HTTP
/// mock both need it.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build a multi-thread tokio runtime")
        .block_on(future)
}

// ---------------------------------------------------------------------------
// A capturing transport: records what it is sent, and can be made to block.
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct CapturingTransport {
    inner: Arc<Captured>,
}

#[derive(Default)]
struct Captured {
    recorded: Mutex<Vec<MetricSample>>,
    blocked: Mutex<bool>,
    gate: tokio::sync::Notify,
}

impl MetricTransport for CapturingTransport {
    async fn send(&self, samples: &[MetricSample]) -> Result<(), PortError> {
        if *self.inner.blocked.lock().unwrap() {
            // A stuck backend: nothing notifies this in the test, so the flush task parks here and
            // the queue behind it fills, which is exactly the saturation the case wants to create.
            self.inner.gate.notified().await;
        }
        self.inner
            .recorded
            .lock()
            .unwrap()
            .extend_from_slice(samples);
        Ok(())
    }
}

impl CapturingTransport {
    fn recorded(&self) -> Vec<MetricSample> {
        self.inner.recorded.lock().unwrap().clone()
    }

    fn block(&self) {
        *self.inner.blocked.lock().unwrap() = true;
    }
}

/// A valid throwaway sample for filling the queue.
fn filler() -> MetricSample {
    MetricSample::new(
        MetricName::parse("pos.saturate.filler").expect("valid name"),
        1,
        MetricUnit::Count,
        fixtures::instant(),
    )
}

struct SinkHarness;

impl MetricsSinkHarness for SinkHarness {
    type Sink = VmMetrics<CapturingTransport>;

    async fn fresh(&self) -> Setup<Self::Sink> {
        Ok(VmMetrics::with_transport(CapturingTransport::default()))
    }

    async fn recorded(&self, sink: &Self::Sink) -> Setup<Vec<MetricSample>> {
        sink.flush()
            .await
            .map_err(|error| HarnessError::new(error.to_string()))?;
        Ok(sink.transport().recorded())
    }

    async fn saturate(&self, sink: &Self::Sink) -> Setup<()> {
        // Stall the transport, then overfill the queue: the flush task pulls one batch and parks in
        // the blocked `send`, and everything past the queue's capacity is dropped by `record`.
        sink.transport().block();
        let filler = filler();
        for _ in 0..(QUEUE_CAPACITY + 8) {
            let _ = sink.record(core::slice::from_ref(&filler)).await;
        }
        Ok(())
    }
}

mod metrics_sink {
    use super::{SinkHarness, block_on};
    pos_contract_tests::metrics_sink_suite!(SinkHarness, block_on);
}

// ---------------------------------------------------------------------------
// The `VictoriaMetrics` JSON import wire, against an in-process mock.
// ---------------------------------------------------------------------------

/// The real `HttpTransport` emits `VictoriaMetrics`' JSON line import, one object per sample, with the
/// unit and labels as `metric` keys and the value and timestamp as single-element arrays.
#[test]
fn emits_the_victoriametrics_json_import_format() {
    block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the mock");
        let addr = listener.local_addr().expect("mock address");

        let captured = Arc::new(Mutex::new(String::new()));
        let sink_body = Arc::clone(&captured);
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept one connection");
            let body = read_request_body(&mut socket).await;
            *sink_body.lock().unwrap() = body;
            socket
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write the response");
            let _ = socket.shutdown().await;
        });

        let sink =
            VmMetrics::connect(&format!("http://{addr}/api/v1/import")).expect("build the sink");
        let sample = MetricSample::new(
            MetricName::parse("pos.outbox.depth").expect("valid name"),
            7,
            MetricUnit::Items,
            fixtures::instant(),
        )
        .with_label(
            MetricLabel::parse("port_name").expect("valid label"),
            MetricLabelValue::parse("event_store").expect("valid value"),
        );
        sink.record(&[sample]).await.expect("record");
        sink.flush().await.expect("flush");
        server.await.expect("mock joins");

        let body = captured.lock().unwrap().clone();
        let epoch = fixtures::instant().as_milliseconds_since_epoch();
        for needle in [
            "\"__name__\":\"pos.outbox.depth\"",
            "\"unit\":\"items\"",
            "\"port_name\":\"event_store\"",
            "\"values\":[7]",
            &format!("\"timestamps\":[{epoch}]"),
        ] {
            assert!(
                body.contains(needle),
                "import body {body:?} is missing {needle:?}"
            );
        }
    });
}

/// Reads one HTTP request and returns its body, honouring `Content-Length`.
async fn read_request_body(socket: &mut TcpStream) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 2048];
    loop {
        if let Some(header_end) = find(&buffer, b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&buffer[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let lower = line.to_ascii_lowercase();
                    lower
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            let body_start = header_end + 4;
            if buffer.len() >= body_start + content_length {
                return String::from_utf8_lossy(&buffer[body_start..body_start + content_length])
                    .into_owned();
            }
        }
        let read = socket.read(&mut chunk).await.expect("read the request");
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

/// The first offset at which `needle` occurs in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
