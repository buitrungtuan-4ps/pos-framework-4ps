// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `blob-garage` against an in-process S3 mock.
//!
//! Runs the shared `BlobStore` contract suite locally with no external server, so the request
//! construction, response parsing, and — the case that matters most — the segment-aware prefix
//! listing are verified in the ordinary `test` job. The mock does naive *string*-prefix matching on
//! `list`, exactly as real S3's `ListObjectsV2` does, so it is the **adapter's** `is_under` filter
//! that must keep `stores/1` from returning `stores/10`. The mock ignores authentication; the `SigV4` signature bytes
//! are proven independently against AWS's vector in `src/sign.rs`, and a real server accepting them
//! is proven by the `integration` suite against MinIO/Garage in CI.

// Test scaffolding: the mock and harness live outside the `#[test]` scope `allow-expect-in-tests`
// covers, and the mock parses a small HTTP request by slicing.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    reason = "test scaffolding: an unrecoverable mock or fixture fault, request slicing in the \
              mock, and a point-free error converter"
)]

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use blob_garage::S3Blobs;
use pos_contract_tests::harness::{BlobStoreHarness, HarnessError, Setup};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Drives a future on a fresh multi-thread runtime with IO enabled.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build a multi-thread tokio runtime")
        .block_on(future)
}

// ---------------------------------------------------------------------------
// The in-process S3 mock: one shared instance for the whole binary.
// ---------------------------------------------------------------------------

/// Objects keyed by their full `bucket/key` path.
type Objects = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;

/// The mock's port, started once on its own thread+runtime the first time it is needed.
static MOCK_PORT: OnceLock<u16> = OnceLock::new();

fn mock_port() -> u16 {
    *MOCK_PORT.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        // A dedicated OS thread with its own runtime, so the mock outlives the per-case runtimes the
        // contract suite builds and tears down.
        std::thread::Builder::new()
            .name("blob-garage-s3-mock".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("build the mock runtime");
                runtime.block_on(async move {
                    let listener = TcpListener::bind("127.0.0.1:0")
                        .await
                        .expect("bind the mock");
                    let port = listener.local_addr().expect("mock address").port();
                    tx.send(port).expect("report the mock port");
                    let objects: Objects = Arc::new(Mutex::new(BTreeMap::new()));
                    loop {
                        let Ok((socket, _)) = listener.accept().await else {
                            continue;
                        };
                        let objects = Arc::clone(&objects);
                        drop(tokio::spawn(handle(socket, objects)));
                    }
                });
            })
            .expect("spawn the mock thread");
        rx.recv().expect("the mock reports its port")
    })
}

/// Handles one request on one connection, then closes it (the client uses `Connection: close`).
async fn handle(mut socket: TcpStream, objects: Objects) {
    let request = read_request(&mut socket).await;
    let (status, body) = route(&request, &objects);
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = socket.write_all(head.as_bytes()).await;
    let _ = socket.write_all(&body).await;
    let _ = socket.shutdown().await;
}

/// A parsed request: method, path, query, and body.
struct Request {
    method: String,
    path: String,
    query: String,
    body: Vec<u8>,
}

/// Reads one HTTP request, honouring `Content-Length`.
async fn read_request(socket: &mut TcpStream) -> Request {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        if let Some(header_end) = find(&buffer, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
            let content_length = head
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
                let request_line = head.lines().next().unwrap_or("");
                let mut parts = request_line.split(' ');
                let method = parts.next().unwrap_or("").to_owned();
                let target = parts.next().unwrap_or("/");
                let (path, query) = match target.split_once('?') {
                    Some((path, query)) => (path.to_owned(), query.to_owned()),
                    None => (target.to_owned(), String::new()),
                };
                let body = buffer[body_start..body_start + content_length].to_vec();
                return Request {
                    method,
                    path,
                    query,
                    body,
                };
            }
        }
        let read = socket.read(&mut chunk).await.expect("read the request");
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    Request {
        method: String::new(),
        path: "/".to_owned(),
        query: String::new(),
        body: Vec::new(),
    }
}

/// Routes a parsed request to a status and response body.
fn route(request: &Request, objects: &Objects) -> (u16, Vec<u8>) {
    // Path is `/bucket` or `/bucket/key...`; strip the leading slash and split off the bucket.
    let without_slash = request.path.strip_prefix('/').unwrap_or(&request.path);
    let (bucket, key) = match without_slash.split_once('/') {
        Some((bucket, key)) => (bucket.to_owned(), Some(key.to_owned())),
        None => (without_slash.to_owned(), None),
    };
    let mut store = objects.lock().unwrap();
    match (request.method.as_str(), key) {
        // Bucket create — idempotent.
        ("PUT", None) => (200, Vec::new()),
        // List — naive string prefix, exactly like real S3, so the adapter's is_under does the work.
        ("GET", None) => {
            use core::fmt::Write as _;
            let prefix = query_value(&request.query, "prefix").unwrap_or_default();
            let mut xml = String::from("<?xml version=\"1.0\"?><ListBucketResult>");
            let bucket_prefix = format!("{bucket}/");
            for full in store.keys() {
                if let Some(object_key) = full.strip_prefix(&bucket_prefix)
                    && object_key.starts_with(&prefix)
                {
                    let _ = write!(xml, "<Contents><Key>{object_key}</Key></Contents>");
                }
            }
            xml.push_str("<IsTruncated>false</IsTruncated></ListBucketResult>");
            (200, xml.into_bytes())
        }
        ("PUT", Some(key)) => {
            store.insert(format!("{bucket}/{key}"), request.body.clone());
            (200, Vec::new())
        }
        ("GET", Some(key)) => match store.get(&format!("{bucket}/{key}")) {
            Some(body) => (200, body.clone()),
            None => (404, Vec::new()),
        },
        ("DELETE", Some(key)) => {
            store.remove(&format!("{bucket}/{key}"));
            (204, Vec::new())
        }
        _ => (400, Vec::new()),
    }
}

/// Reads a query parameter, percent-decoding its value (the adapter encodes `/` as `%2F`).
fn query_value(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        pair.split_once('=').and_then(|(key, value)| {
            if key == name {
                Some(percent_decode(value))
            } else {
                None
            }
        })
    })
}

/// Decodes `%XX` escapes; leaves everything else as-is.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &input[index + 1..index + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn port_err(error: pos_ports::PortError) -> HarnessError {
    HarnessError::new(error.to_string())
}

// ---------------------------------------------------------------------------
// The harness: a fresh bucket per case, against the shared mock.
// ---------------------------------------------------------------------------

/// Global across every case, because the suite builds a fresh harness per test but they share the
/// one mock — a per-harness counter would restart at 0 and collide buckets between cases.
static NEXT_BUCKET: AtomicU64 = AtomicU64::new(0);

struct MockHarness;

impl MockHarness {
    fn new() -> Self {
        Self
    }
}

impl BlobStoreHarness for MockHarness {
    type Store = S3Blobs;

    async fn fresh(&self) -> Setup<S3Blobs> {
        let n = NEXT_BUCKET.fetch_add(1, Ordering::Relaxed);
        let endpoint = format!("http://127.0.0.1:{}", mock_port());
        let store = S3Blobs::new(
            &endpoint,
            &format!("bucket{n}"),
            "us-east-1",
            "key",
            "secret",
        )
        .map_err(port_err)?;
        Ok(store)
    }
}

mod blob_store {
    use super::{MockHarness, block_on};
    pos_contract_tests::blob_store_suite!(MockHarness::new(), block_on);
}
