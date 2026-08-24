// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `ErpSink` contract suite, against `HttpSapErp` over a stateful stub ERP.
//!
//! `docs/roadmap.md` P2's exit criterion is *"every port has a contract suite; every implementation
//! passes it"*, and P11's is *"every adapter passes its port's contract suite"*. This is the
//! `erp-sap` half. The stub ERP reproduces the ERP's exact status/body responses (ADR-0059) and
//! remembers what has posted, keyed by `(store, business_date)`, so the suite checks the adapter's
//! whole branching — the batch it shapes, the idempotency and supersession it relies on, and the
//! [`PortError`](pos_ports::PortError) it maps each status to — in the fast pull-request gate, with no
//! socket. The real TLS path (`TlsErpTransport`) is exercised in the gated integration lane.

// The whole file is test scaffolding. `allow-expect-in-tests` in clippy.toml scopes to `#[test]` and
// `#[cfg(test)]`, which does not reach an integration test's module-level helpers, so the stub ERP and
// the runtime it drives are allowed to expect and to take the trait's owned body by value here.
#![allow(
    clippy::expect_used,
    clippy::needless_pass_by_value,
    reason = "test scaffolding: a stub ERP whose own state is corrupt is an unrecoverable test-setup \
              fault, not a contract failure; and the ErpTransport method takes its body by owned \
              Option<Vec>, which the stub only needs to read"
)]

use core::future::Future;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use erp_sap::{ErpTransport, HttpResponse, HttpSapErp, Method, TransportError};
use pos_contract_tests::harness::{ErpSinkHarness, Setup};
use pos_ports::AccountCode;
use pos_proto::{StoreId, Ulid};

/// The one account code the stub ERP knows — a SAP-ish general-ledger account.
const KNOWN_ACCOUNT: &str = "0000501100";
/// An account code it does not.
const UNKNOWN_ACCOUNT: &str = "9999999999";

/// The store the cases post for.
fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(0xE_2B))
}

/// The stub ERP's memory: one posting per `(store, business_date)`, replaced by a higher revision
/// rather than appended to — the obligation with the worst failure attached, since an ERP that
/// appended would double-count a reposted day.
#[derive(Default)]
struct ErpState {
    posted: BTreeMap<(String, String), u32>,
}

/// A stub ERP speaking the exact wire the adapter targets (ADR-0059). Cloneable over shared state so
/// the harness can hand out an adapter while the state persists across the calls one case makes.
#[derive(Clone, Default)]
struct StubSap {
    state: Arc<Mutex<ErpState>>,
}

impl StubSap {
    fn posting(store: &str, date: &str, revision: u32) -> Vec<u8> {
        format!(r#"{{"document_ref":"DOC-{store}-{date}-{revision}","revision":{revision}}}"#)
            .into_bytes()
    }

    fn post(&self, body: &[u8]) -> HttpResponse {
        let request: serde_json::Value =
            serde_json::from_slice(body).expect("the adapter sends a JSON batch body");
        let store = request
            .get("store_id")
            .and_then(serde_json::Value::as_str)
            .expect("a store_id")
            .to_owned();
        let date = request
            .get("business_date")
            .and_then(serde_json::Value::as_str)
            .expect("a business_date")
            .to_owned();
        let revision = u32::try_from(
            request
                .get("revision")
                .and_then(serde_json::Value::as_u64)
                .expect("a revision"),
        )
        .expect("a revision within u32");
        let lines = request
            .get("lines")
            .and_then(serde_json::Value::as_array)
            .expect("a lines array");

        // Validated before anything is written: a batch is posted whole or not at all.
        let all_known = lines.iter().all(|line| {
            line.get("account_code").and_then(serde_json::Value::as_str) == Some(KNOWN_ACCOUNT)
        });
        if !all_known {
            return HttpResponse {
                status: 400,
                body: b"unknown account code".to_vec(),
            };
        }

        let mut state = self.state.lock().expect("stub ERP lock");
        let key = (store.clone(), date.clone());
        match state.posted.get(&key).copied() {
            // A later revision already posted: the port learns the repost was unnecessary.
            Some(existing) if existing > revision => HttpResponse {
                status: 409,
                body: b"a later revision of this day has already posted".to_vec(),
            },
            // The same revision again: success with the same document, so a retried job is harmless.
            Some(existing) if existing == revision => HttpResponse {
                status: 200,
                body: Self::posting(&store, &date, revision),
            },
            // None yet, or a lower revision this one supersedes. Insert replaces — appending here is
            // what would double-count a reposted day.
            _new_or_supersedes => {
                state.posted.insert(key, revision);
                HttpResponse {
                    status: 201,
                    body: Self::posting(&store, &date, revision),
                }
            }
        }
    }

    fn posted(&self, query: &str) -> HttpResponse {
        let mut store = "";
        let mut date = "";
        for pair in query.split('&') {
            match pair.split_once('=') {
                Some(("store_id", value)) => store = value,
                Some(("business_date", value)) => date = value,
                _other => {}
            }
        }
        let state = self.state.lock().expect("stub ERP lock");
        state
            .posted
            .get(&(store.to_owned(), date.to_owned()))
            .map_or_else(
                || HttpResponse {
                    status: 404,
                    body: b"nothing posted for this day".to_vec(),
                },
                |revision| HttpResponse {
                    status: 200,
                    body: Self::posting(store, date, *revision),
                },
            )
    }
}

impl ErpTransport for StubSap {
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<HttpResponse, TransportError> {
        let response = match method {
            Method::Post => self.post(&body.expect("a posting carries a body")),
            Method::Get => {
                let query = path.split_once('?').map_or("", |(_path, query)| query);
                self.posted(query)
            }
        };
        Ok(response)
    }
}

/// The harness the shared suite runs against: `HttpSapErp` over one stub ERP whose state persists for
/// the case's lifetime.
struct ErpSapHarness {
    stub: StubSap,
}

impl ErpSapHarness {
    fn new() -> Self {
        Self {
            stub: StubSap::default(),
        }
    }
}

impl ErpSinkHarness for ErpSapHarness {
    type Erp = HttpSapErp<StubSap>;

    async fn fresh(&self) -> Setup<Self::Erp> {
        Ok(HttpSapErp::new(self.stub.clone()))
    }

    fn known_account(&self) -> AccountCode {
        AccountCode::new(KNOWN_ACCOUNT)
    }

    fn unknown_account(&self) -> AccountCode {
        AccountCode::new(UNKNOWN_ACCOUNT)
    }

    fn store_id(&self) -> StoreId {
        store_id()
    }
}

/// Drives a future to completion. The stub is immediately ready and touches no socket, so a
/// current-thread runtime is all the suite needs.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build a current-thread tokio runtime")
        .block_on(future)
}

mod erp {
    use super::{ErpSapHarness, block_on};
    pos_contract_tests::erp_sink_suite!(ErpSapHarness::new(), block_on);
}
