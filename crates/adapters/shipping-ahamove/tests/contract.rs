// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `ShippingDispatch` contract suite, against `HttpAhamove` over a stateful stub courier.
//!
//! `docs/roadmap.md` P2's exit criterion is *"every port has a contract suite; every implementation
//! passes it"*, and P11's is *"every adapter passes its port's contract suite"*. This is the
//! `shipping-ahamove` half. The stub courier reproduces the courier's exact status/body responses
//! (ADR-0058) and — unlike a stateless request/response cloud — remembers the jobs it has booked, so
//! the suite checks the adapter's whole branching behaviour (the request it shapes, the idempotency it
//! relies on, and the [`PortError`](pos_ports::PortError) it maps each status to) in the fast
//! pull-request gate, with no socket. The real TLS path (`TlsCourierTransport`) is exercised in the
//! gated integration lane and the soak.

// The whole file is test scaffolding. `allow-expect-in-tests` in clippy.toml scopes to `#[test]` and
// `#[cfg(test)]`, which does not reach an integration test's module-level helpers, so the stub courier
// and the runtime it drives are allowed to expect and to take the trait's owned body by value here.
#![allow(
    clippy::expect_used,
    clippy::needless_pass_by_value,
    reason = "test scaffolding: a stub courier whose own state is corrupt is an unrecoverable \
              test-setup fault, not a contract failure; and the CourierTransport method takes its \
              body by owned Option<Vec>, which the stub only needs to read"
)]

use core::future::Future;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pos_contract_tests::harness::{Setup, ShippingDispatchHarness};
use pos_ports::shipping::CourierJobRef;
use pos_proto::{StoreId, Ulid};
use shipping_ahamove::{CourierTransport, HttpAhamove, HttpResponse, Method, TransportError};

/// The store the cases book against.
fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(0x5_709E))
}

/// One booked job as the stub courier remembers it.
#[derive(Clone)]
struct Job {
    shipment_id: String,
    status: &'static str,
}

/// The stub courier's memory: it books jobs, dedupes by the idempotency key we send, and answers
/// cancel and track from what it is holding — the state a real courier keeps that a stateless stub
/// could not model.
#[derive(Default)]
struct CourierState {
    by_key: BTreeMap<String, String>,
    jobs: BTreeMap<String, Job>,
    next: u32,
}

/// A stub courier speaking the exact wire the adapter targets (ADR-0058). Cloneable over shared state
/// so the harness that hands it out can also complete a job behind its back — a courier delivering is
/// an external event, exactly as `FakeShipping::complete` models it.
#[derive(Clone, Default)]
struct StubAhamove {
    state: Arc<Mutex<CourierState>>,
}

impl StubAhamove {
    fn body(job_ref: &str, job: &Job) -> Vec<u8> {
        format!(
            r#"{{"shipment_id":"{}","courier_job_ref":"{job_ref}","status":"{}",
                 "fee_minor":25000,"fee_currency":"VND","updated_at_ms":0}}"#,
            job.shipment_id, job.status
        )
        .into_bytes()
    }

    fn create(&self, body: &[u8]) -> HttpResponse {
        let request: serde_json::Value =
            serde_json::from_slice(body).expect("the adapter sends a JSON booking body");
        let key = request
            .get("idempotency_key")
            .and_then(serde_json::Value::as_str)
            .expect("the adapter sends an idempotency_key")
            .to_owned();
        let mut state = self.state.lock().expect("stub courier lock");
        // One idempotency key, one rider: a retry after a timeout returns the same job.
        if let Some(existing) = state.by_key.get(&key).cloned() {
            let job = state.jobs.get(&existing).expect("a booked job").clone();
            return HttpResponse {
                status: 200,
                body: Self::body(&existing, &job),
            };
        }
        state.next = state.next.saturating_add(1);
        let job_ref = format!("AHA-{}", state.next);
        let job = Job {
            shipment_id: key.clone(),
            status: "ACCEPTED",
        };
        state.by_key.insert(key, job_ref.clone());
        state.jobs.insert(job_ref.clone(), job.clone());
        HttpResponse {
            status: 201,
            body: Self::body(&job_ref, &job),
        }
    }

    fn cancel(&self, job_ref: &str) -> HttpResponse {
        let mut state = self.state.lock().expect("stub courier lock");
        let Some(job) = state.jobs.get(job_ref).cloned() else {
            return HttpResponse {
                status: 404,
                body: b"no such job".to_vec(),
            };
        };
        match job.status {
            // Delivered cannot be un-delivered; a successful-looking cancel would promise a refund.
            "COMPLETED" => HttpResponse {
                status: 409,
                body: b"already delivered".to_vec(),
            },
            // Cancellation is retried, so a repeat is a success with the current view.
            "CANCELLED" => HttpResponse {
                status: 200,
                body: Self::body(job_ref, &job),
            },
            _live => {
                let cancelled = Job {
                    status: "CANCELLED",
                    ..job
                };
                state.jobs.insert(job_ref.to_owned(), cancelled.clone());
                HttpResponse {
                    status: 200,
                    body: Self::body(job_ref, &cancelled),
                }
            }
        }
    }

    fn track(&self, job_ref: &str) -> HttpResponse {
        let state = self.state.lock().expect("stub courier lock");
        state.jobs.get(job_ref).map_or_else(
            || HttpResponse {
                status: 404,
                body: b"no such job".to_vec(),
            },
            |job| HttpResponse {
                status: 200,
                body: Self::body(job_ref, job),
            },
        )
    }
}

impl CourierTransport for StubAhamove {
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<HttpResponse, TransportError> {
        let rest = path.strip_prefix("/v1/shipments").unwrap_or(path);
        let response = match (method, rest) {
            (Method::Post, "") => self.create(&body.expect("a booking carries a body")),
            (Method::Post, sub) if sub.ends_with("/cancel") => {
                let job_ref = sub.trim_start_matches('/').trim_end_matches("/cancel");
                self.cancel(job_ref)
            }
            (Method::Get, sub) if !sub.is_empty() => self.track(sub.trim_start_matches('/')),
            _unrouted => HttpResponse {
                status: 404,
                body: format!("the stub courier has no route {method:?} {path}").into_bytes(),
            },
        };
        Ok(response)
    }
}

/// The harness the shared suite runs against: `HttpAhamove` over one stub courier whose state the
/// harness also holds, so it can complete a job for the "cannot cancel a completed job" obligation.
struct AhamoveHarness {
    stub: StubAhamove,
}

impl AhamoveHarness {
    fn new() -> Self {
        Self {
            stub: StubAhamove::default(),
        }
    }
}

impl ShippingDispatchHarness for AhamoveHarness {
    type Courier = HttpAhamove<StubAhamove>;

    async fn fresh(&self) -> Setup<Self::Courier> {
        Ok(HttpAhamove::new(self.stub.clone()))
    }

    async fn complete(&self, _courier: &Self::Courier, job: &CourierJobRef) -> Setup<()> {
        let mut state = self.stub.state.lock().expect("stub courier lock");
        if let Some(existing) = state.jobs.get(job.as_str()).cloned() {
            state.jobs.insert(
                job.as_str().to_owned(),
                Job {
                    status: "COMPLETED",
                    ..existing
                },
            );
        }
        Ok(())
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

mod shipping {
    use super::{AhamoveHarness, block_on};
    pos_contract_tests::shipping_dispatch_suite!(AhamoveHarness::new(), block_on);
}
