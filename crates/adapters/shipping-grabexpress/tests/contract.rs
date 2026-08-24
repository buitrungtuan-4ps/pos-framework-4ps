// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `ShippingDispatch` contract suite, against `HttpGrabExpress` over a stateful stub courier.
//!
//! `docs/roadmap.md` P11's exit criterion is *"every adapter passes its port's contract suite"*. This
//! is the `shipping-grabexpress` half, and it is the same suite `shipping-ahamove` passes against the
//! same stub shape — the whole point of the `templates/adapter-template` extraction is that a second
//! courier is this little new code. The stub courier reproduces Grab Express's exact status/body
//! responses (ADR-0058) and remembers the jobs it has booked, so the suite checks the adapter's
//! branching in the fast pull-request gate with no socket. The real TLS path (`TlsCourierTransport`)
//! is exercised in the gated integration lane.

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
use shipping_grabexpress::{
    CourierTransport, HttpGrabExpress, HttpResponse, Method, TransportError,
};

/// The store the cases book against.
fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(0x6_4A8))
}

/// One booked job as the stub courier remembers it.
#[derive(Clone)]
struct Job {
    merchant_order_id: String,
    status: &'static str,
}

/// The stub courier's memory: it books jobs, dedupes by the `merchant_order_id` we send, and answers
/// cancel and track from what it is holding.
#[derive(Default)]
struct CourierState {
    by_order: BTreeMap<String, String>,
    jobs: BTreeMap<String, Job>,
    next: u32,
}

/// A stub courier speaking the exact Grab Express wire the adapter targets (ADR-0058). Cloneable over
/// shared state so the harness can also complete a job behind its back — a courier delivering is an
/// external event.
#[derive(Clone, Default)]
struct StubGrabExpress {
    state: Arc<Mutex<CourierState>>,
}

impl StubGrabExpress {
    fn body(delivery_id: &str, job: &Job) -> Vec<u8> {
        format!(
            r#"{{"merchant_order_id":"{}","delivery_id":"{delivery_id}","status":"{}",
                 "fee_minor":30000,"fee_currency":"VND","updated_at_ms":0}}"#,
            job.merchant_order_id, job.status
        )
        .into_bytes()
    }

    fn create(&self, body: &[u8]) -> HttpResponse {
        let request: serde_json::Value =
            serde_json::from_slice(body).expect("the adapter sends a JSON booking body");
        let order = request
            .get("merchant_order_id")
            .and_then(serde_json::Value::as_str)
            .expect("the adapter sends a merchant_order_id")
            .to_owned();
        let mut state = self.state.lock().expect("stub courier lock");
        // One merchant order id, one rider: a retry after a timeout returns the same job.
        if let Some(existing) = state.by_order.get(&order).cloned() {
            let job = state.jobs.get(&existing).expect("a booked job").clone();
            return HttpResponse {
                status: 200,
                body: Self::body(&existing, &job),
            };
        }
        state.next = state.next.saturating_add(1);
        let delivery_id = format!("GX-{}", state.next);
        let job = Job {
            merchant_order_id: order.clone(),
            status: "ALLOCATING",
        };
        state.by_order.insert(order, delivery_id.clone());
        state.jobs.insert(delivery_id.clone(), job.clone());
        HttpResponse {
            status: 201,
            body: Self::body(&delivery_id, &job),
        }
    }

    fn cancel(&self, delivery_id: &str) -> HttpResponse {
        let mut state = self.state.lock().expect("stub courier lock");
        let Some(job) = state.jobs.get(delivery_id).cloned() else {
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
            "CANCELED" => HttpResponse {
                status: 200,
                body: Self::body(delivery_id, &job),
            },
            _live => {
                let cancelled = Job {
                    status: "CANCELED",
                    ..job
                };
                state.jobs.insert(delivery_id.to_owned(), cancelled.clone());
                HttpResponse {
                    status: 200,
                    body: Self::body(delivery_id, &cancelled),
                }
            }
        }
    }

    fn track(&self, delivery_id: &str) -> HttpResponse {
        let state = self.state.lock().expect("stub courier lock");
        state.jobs.get(delivery_id).map_or_else(
            || HttpResponse {
                status: 404,
                body: b"no such job".to_vec(),
            },
            |job| HttpResponse {
                status: 200,
                body: Self::body(delivery_id, job),
            },
        )
    }
}

impl CourierTransport for StubGrabExpress {
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<HttpResponse, TransportError> {
        let rest = path.strip_prefix("/v1/deliveries").unwrap_or(path);
        let response = match (method, rest) {
            (Method::Post, "") => self.create(&body.expect("a booking carries a body")),
            (Method::Post, sub) if sub.ends_with("/cancel") => {
                let delivery_id = sub.trim_start_matches('/').trim_end_matches("/cancel");
                self.cancel(delivery_id)
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

/// The harness the shared suite runs against: `HttpGrabExpress` over one stub courier whose state the
/// harness also holds, so it can complete a job for the "cannot cancel a completed job" obligation.
struct GrabExpressHarness {
    stub: StubGrabExpress,
}

impl GrabExpressHarness {
    fn new() -> Self {
        Self {
            stub: StubGrabExpress::default(),
        }
    }
}

impl ShippingDispatchHarness for GrabExpressHarness {
    type Courier = HttpGrabExpress<StubGrabExpress>;

    async fn fresh(&self) -> Setup<Self::Courier> {
        Ok(HttpGrabExpress::new(self.stub.clone()))
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
    use super::{GrabExpressHarness, block_on};
    pos_contract_tests::shipping_dispatch_suite!(GrabExpressHarness::new(), block_on);
}
