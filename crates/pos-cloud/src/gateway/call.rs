// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! One call through the gateway
//! ([ADR-0198](../../../../docs/adr/0198-a-store-reaches-the-internet-only-through-the-cloud.md)
//! decisions 2 and 4 to 6): a store's values checked against the endpoint's request fields, the
//! request built from them by name, sent within the endpoint's deadline (retried at most twice, and
//! only for an endpoint declared idempotent), and the answer read into the endpoint's response
//! fields, everything else dropped.
//!
//! The network is a [`GatewayTransport`], so the rules are proved here against a fake. The real one
//! is the crate's outbound HTTPS client (`outbound.rs`, shared with the release source and the HQ
//! federation): it vets the URL with the webhook guard before every attempt, dials only the vetted
//! addresses, follows no redirect and reads at most [`MAX_ANSWER_BYTES`].
//!
//! Nothing here logs a value or a body: the route that drives a call logs its id, the endpoint's id,
//! the outcome and how long it took (decision 9).

use core::future::Future;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::endpoints::{GatewayEndpoint, GatewayField, GatewayFieldKind, GatewayMethod};

/// The most bytes an answer may carry (decision 4).
pub const MAX_ANSWER_BYTES: usize = 256 * 1024;
/// The most retries after the first attempt, for an idempotent endpoint (decision 6).
pub const MAX_RETRIES: u32 = 2;
/// The longest text value a request may carry, in characters.
pub const MAX_TEXT_CHARS: usize = 1_024;
/// The longest idempotency key a store may send, in bytes.
pub const MAX_IDEMPOTENCY_KEY: usize = 64;

/// The request the gateway sends: everything the transport needs, secrets included, so it is never
/// printed: [`core::fmt::Debug`] names only the method and how many headers it carries.
pub struct OutboundRequest {
    /// The method.
    pub method: GatewayMethod,
    /// The URL, with the values as its query for a `GET`.
    pub url: String,
    /// The headers after `Host`: the endpoint's opened secret headers, `Idempotency-Key`, and
    /// `Content-Type` for a body.
    pub headers: Vec<(String, String)>,
    /// The JSON body of a `POST`.
    pub body: Option<Vec<u8>>,
}

impl core::fmt::Debug for OutboundRequest {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OutboundRequest")
            .field("method", &self.method)
            .field("headers", &self.headers.len())
            .finish_non_exhaustive()
    }
}

/// What one attempt answered: its status and body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetAnswer {
    /// The HTTP status.
    pub status: u16,
    /// The body, at most [`MAX_ANSWER_BYTES`].
    pub body: Vec<u8>,
}

/// Sends one request. An `Err` is a transport failure: the vet refused the address, the dial, the
/// TLS or HTTP exchange failed, or the answer was over the limit. Its text goes to no response.
pub trait GatewayTransport {
    /// Sends `request` once.
    fn send(
        &self,
        request: &OutboundRequest,
    ) -> impl Future<Output = Result<TargetAnswer, String>> + Send;
}

/// Why a call answered no result, as the store-facing route names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub enum CallRefusal {
    /// A value is missing, undeclared, or of the wrong kind; checked before anything is sent.
    #[serde(rename = "GATEWAY_REFUSAL_INVALID_VALUES")]
    InvalidValues,
    /// The deadline passed before an answer.
    #[serde(rename = "GATEWAY_REFUSAL_TIMEOUT")]
    Timeout,
    /// The target could not be reached, or its answer was too large.
    #[serde(rename = "GATEWAY_REFUSAL_UNREACHABLE")]
    Unreachable,
    /// The target answered with a status other than `2xx`.
    #[serde(rename = "GATEWAY_REFUSAL_TARGET_ERROR")]
    TargetError,
    /// The target's answer was not a JSON object holding the declared fields.
    #[serde(rename = "GATEWAY_REFUSAL_MALFORMED_ANSWER")]
    MalformedAnswer,
}

/// How a call ended.
#[derive(Clone, Debug, PartialEq)]
pub enum CallOutcome {
    /// The target answered `2xx` with the declared fields, read into `result`.
    Answered {
        /// The target's status.
        status: u16,
        /// The declared response fields, by name, each of its kind.
        result: Map<String, Value>,
    },
    /// No result; `status` is the target's, where it answered one.
    Refused {
        /// Why.
        refusal: CallRefusal,
        /// The target's status, when it answered.
        status: Option<u16>,
    },
}

/// A refused value: the field it names and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueRefusal {
    /// The field, or empty for the request as a whole.
    pub field: String,
    /// `UNKNOWN_FIELD`, `REQUIRED`, `WRONG_KIND` or `TOO_LONG`.
    pub reason: &'static str,
}

/// Whether `key` is an idempotency key a store may send: 1 to 64 letters, digits, `-` and `_`.
#[must_use]
pub fn is_idempotency_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_IDEMPOTENCY_KEY
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Whether `value` is of `kind`.
fn of_kind(value: &Value, kind: GatewayFieldKind) -> bool {
    match kind {
        GatewayFieldKind::String => value.is_string(),
        GatewayFieldKind::Integer => value.is_i64(),
        GatewayFieldKind::Boolean => value.is_boolean(),
    }
}

fn declared<'a>(fields: &'a [GatewayField], name: &str) -> Option<&'a GatewayField> {
    fields.iter().find(|field| field.name == name)
}

/// Checks a store's values against the endpoint's request fields: each one declared, of its kind,
/// text within [`MAX_TEXT_CHARS`], and every required field present.
///
/// # Errors
///
/// The first [`ValueRefusal`].
pub fn check_values(
    endpoint: &GatewayEndpoint,
    values: &Map<String, Value>,
) -> Result<(), ValueRefusal> {
    let refuse = |field: &str, reason| ValueRefusal {
        field: field.to_owned(),
        reason,
    };
    for (name, value) in values {
        let field = declared(&endpoint.request_fields, name)
            .ok_or_else(|| refuse(name, "UNKNOWN_FIELD"))?;
        if !of_kind(value, field.kind) {
            return Err(refuse(name, "WRONG_KIND"));
        }
        if value
            .as_str()
            .is_some_and(|text| text.chars().count() > MAX_TEXT_CHARS)
        {
            return Err(refuse(name, "TOO_LONG"));
        }
    }
    if let Some(missing) = endpoint
        .request_fields
        .iter()
        .find(|field| field.required && !values.contains_key(&field.name))
    {
        return Err(refuse(&missing.name, "REQUIRED"));
    }
    Ok(())
}

/// Builds the request: a `GET`'s values as its query, a `POST`'s as a JSON object; the opened
/// secret headers, then `Idempotency-Key` (decision 6).
///
/// # Errors
///
/// A sentence if the endpoint's URL no longer parses, which a stored, vetted URL always does.
pub fn build_request(
    endpoint: &GatewayEndpoint,
    values: &Map<String, Value>,
    idempotency_key: &str,
    secret_headers: Vec<(String, String)>,
) -> Result<OutboundRequest, String> {
    let mut url = url::Url::parse(&endpoint.url).map_err(|error| error.to_string())?;
    let mut headers = secret_headers;
    headers.push(("idempotency-key".to_owned(), idempotency_key.to_owned()));
    let body = match endpoint.method {
        GatewayMethod::Get => {
            if !values.is_empty() {
                let mut query = url.query_pairs_mut();
                for (name, value) in values {
                    let text = value
                        .as_str()
                        .map_or_else(|| value.to_string(), ToOwned::to_owned);
                    query.append_pair(name, &text);
                }
            }
            None
        }
        GatewayMethod::Post => {
            headers.push(("content-type".to_owned(), "application/json".to_owned()));
            Some(serde_json::to_vec(values).map_err(|error| error.to_string())?)
        }
    };
    Ok(OutboundRequest {
        method: endpoint.method,
        url: url.to_string(),
        headers,
        body,
    })
}

/// Reads a `2xx` answer into the endpoint's response fields: a JSON object, each declared field of
/// its kind where present, every required one present; anything else it answers is dropped.
///
/// # Errors
///
/// [`CallRefusal::MalformedAnswer`].
pub fn read_answer(
    endpoint: &GatewayEndpoint,
    body: &[u8],
) -> Result<Map<String, Value>, CallRefusal> {
    let Ok(Value::Object(answered)) = serde_json::from_slice::<Value>(body) else {
        return Err(CallRefusal::MalformedAnswer);
    };
    let mut result = Map::new();
    for field in &endpoint.response_fields {
        match answered.get(&field.name) {
            Some(value) if of_kind(value, field.kind) => {
                result.insert(field.name.clone(), value.clone());
            }
            Some(Value::Null) | None if !field.required => {}
            _ => return Err(CallRefusal::MalformedAnswer),
        }
    }
    Ok(result)
}

/// Whether an attempt's result may be retried: a transport failure or a `5xx`.
fn retryable(attempt: &Result<TargetAnswer, String>) -> bool {
    match attempt {
        Err(_) => true,
        Ok(answer) => answer.status >= 500,
    }
}

/// Sends `request` within `deadline`: once, or for an idempotent endpoint up to [`MAX_RETRIES`]
/// more times after a transport failure or a `5xx` while the deadline allows. The answer is read
/// into the endpoint's response fields.
pub async fn send_within<T: GatewayTransport + Sync>(
    transport: &T,
    endpoint: &GatewayEndpoint,
    request: &OutboundRequest,
    deadline: Duration,
) -> CallOutcome {
    let attempts = if endpoint.idempotent {
        MAX_RETRIES + 1
    } else {
        1
    };
    // One deadline over every attempt together: no clock is read, and a retry never outlives it.
    let run = async {
        let mut last = None;
        for _ in 0..attempts {
            let attempt = transport.send(request).await;
            let again = retryable(&attempt);
            last = Some(attempt);
            if !again {
                break;
            }
        }
        last
    };
    let Ok(last) = tokio::time::timeout(deadline, run).await else {
        return CallOutcome::Refused {
            refusal: CallRefusal::Timeout,
            status: None,
        };
    };
    match last {
        None => CallOutcome::Refused {
            refusal: CallRefusal::Timeout,
            status: None,
        },
        Some(Err(_)) => CallOutcome::Refused {
            refusal: CallRefusal::Unreachable,
            status: None,
        },
        Some(Ok(answer)) if !(200..300).contains(&answer.status) => CallOutcome::Refused {
            refusal: CallRefusal::TargetError,
            status: Some(answer.status),
        },
        Some(Ok(answer)) => match read_answer(endpoint, &answer.body) {
            Ok(result) => CallOutcome::Answered {
                status: answer.status,
                result,
            },
            Err(refusal) => CallOutcome::Refused {
                refusal,
                status: Some(answer.status),
            },
        },
    }
}

impl GatewayTransport for crate::outbound::HttpsGet {
    async fn send(&self, request: &OutboundRequest) -> Result<TargetAnswer, String> {
        let mut headers = Vec::with_capacity(request.headers.len());
        for (name, value) in &request.headers {
            let name = hyper::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| "an endpoint header name is not a header token".to_owned())?;
            headers.push((name, value.clone()));
        }
        let method = match request.method {
            GatewayMethod::Get => "GET",
            GatewayMethod::Post => "POST",
        };
        // The subject names no URL: an endpoint URL may carry a token in its query.
        let answer = self
            .send_once(
                method,
                &request.url,
                &headers,
                request.body.clone(),
                MAX_ANSWER_BYTES,
                "a gateway endpoint",
            )
            .await?;
        Ok(TargetAnswer {
            status: answer.status,
            body: answer.body,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use serde_json::{Map, Value, json};

    use super::{
        CallOutcome, CallRefusal, GatewayTransport, OutboundRequest, TargetAnswer, build_request,
        check_values, is_idempotency_key, read_answer, send_within,
    };
    use crate::gateway::endpoints::{
        GatewayEndpoint, GatewayField, GatewayFieldKind, GatewayMethod,
    };

    fn field(name: &str, kind: GatewayFieldKind, required: bool) -> GatewayField {
        GatewayField {
            name: name.to_owned(),
            kind,
            required,
        }
    }

    fn endpoint(method: GatewayMethod, idempotent: bool) -> GatewayEndpoint {
        GatewayEndpoint {
            gateway_endpoint_id: "01J00000000000000000000G01".to_owned(),
            display_name: "Demo loyalty".to_owned(),
            url: "https://93.184.216.34/balance?brand=demo".to_owned(),
            method,
            request_fields: vec![
                field("card_code", GatewayFieldKind::String, true),
                field("visits", GatewayFieldKind::Integer, false),
            ],
            response_fields: vec![
                field("points", GatewayFieldKind::Integer, true),
                field("tier", GatewayFieldKind::String, false),
            ],
            deadline_ms: 3_000,
            idempotent,
            enabled: true,
            sealed_headers: std::collections::BTreeMap::new(),
        }
    }

    fn values(value: &Value) -> Map<String, Value> {
        value.as_object().cloned().expect("an object")
    }

    #[test]
    fn values_are_declared_of_their_kind_and_required_ones_present() {
        let get = endpoint(GatewayMethod::Get, false);
        assert!(check_values(&get, &values(&json!({ "card_code": "C-1", "visits": 3 }))).is_ok());
        let reason =
            |sent: Value| check_values(&get, &values(&sent)).map_err(|r| (r.field, r.reason));
        assert_eq!(reason(json!({})), Err(("card_code".to_owned(), "REQUIRED")));
        assert_eq!(
            reason(json!({ "card_code": 7 })),
            Err(("card_code".to_owned(), "WRONG_KIND"))
        );
        assert_eq!(
            reason(json!({ "card_code": "C", "visits": 1.5 })),
            Err(("visits".to_owned(), "WRONG_KIND"))
        );
        assert_eq!(
            reason(json!({ "card_code": "C", "phone": "x" })),
            Err(("phone".to_owned(), "UNKNOWN_FIELD"))
        );
        assert_eq!(
            reason(json!({ "card_code": "x".repeat(1_025) })),
            Err(("card_code".to_owned(), "TOO_LONG"))
        );
        assert!(is_idempotency_key("01J0000-act_7"));
        assert!(!is_idempotency_key(""));
        assert!(!is_idempotency_key("has space"));
        assert!(!is_idempotency_key(&"k".repeat(65)));
    }

    #[test]
    fn a_get_carries_its_values_as_the_query_and_a_post_as_json() {
        let sent = values(&json!({ "card_code": "C 1&2", "visits": 3 }));
        let get = build_request(
            &endpoint(GatewayMethod::Get, false),
            &sent,
            "key-1",
            vec![("x-api-key".to_owned(), "secret".to_owned())],
        )
        .expect("builds");
        assert_eq!(
            get.url,
            "https://93.184.216.34/balance?brand=demo&card_code=C+1%262&visits=3"
        );
        assert!(get.body.is_none());
        assert!(
            get.headers
                .contains(&("idempotency-key".to_owned(), "key-1".to_owned()))
        );
        assert!(
            !format!("{get:?}").contains("secret"),
            "Debug prints no header value"
        );

        let post = build_request(
            &endpoint(GatewayMethod::Post, false),
            &sent,
            "key-1",
            Vec::new(),
        )
        .expect("builds");
        assert_eq!(post.url, "https://93.184.216.34/balance?brand=demo");
        let body: Value = serde_json::from_slice(&post.body.expect("a body")).expect("json");
        assert_eq!(body, json!({ "card_code": "C 1&2", "visits": 3 }));
        assert!(
            post.headers
                .contains(&("content-type".to_owned(), "application/json".to_owned()))
        );
    }

    #[test]
    fn an_answer_keeps_only_the_declared_fields_of_their_kind() {
        let declared = endpoint(GatewayMethod::Get, false);
        let read = |body: &str| read_answer(&declared, body.as_bytes());
        assert_eq!(
            read(r#"{"points": 120, "tier": "gold", "phone": "0900"}"#),
            Ok(values(&json!({ "points": 120, "tier": "gold" }))),
            "an undeclared field, personal or not, is dropped"
        );
        assert_eq!(
            read(r#"{"points": 5, "tier": null}"#),
            Ok(values(&json!({ "points": 5 })))
        );
        assert_eq!(
            read(r#"{"tier": "gold"}"#),
            Err(CallRefusal::MalformedAnswer)
        );
        assert_eq!(
            read(r#"{"points": "5"}"#),
            Err(CallRefusal::MalformedAnswer)
        );
        assert_eq!(read("[1]"), Err(CallRefusal::MalformedAnswer));
        assert_eq!(read("not json"), Err(CallRefusal::MalformedAnswer));
    }

    /// Answers each attempt from a script, and counts the attempts.
    struct Scripted {
        answers: Mutex<Vec<Result<TargetAnswer, String>>>,
        delay: Duration,
        attempts: AtomicU32,
    }

    impl Scripted {
        fn new(answers: Vec<Result<TargetAnswer, String>>, delay: Duration) -> Self {
            Self {
                answers: Mutex::new(answers),
                delay,
                attempts: AtomicU32::new(0),
            }
        }
    }

    impl GatewayTransport for Scripted {
        async fn send(&self, _request: &OutboundRequest) -> Result<TargetAnswer, String> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            let mut answers = self.answers.lock().expect("lock");
            if answers.is_empty() {
                Err("no more answers".to_owned())
            } else {
                answers.remove(0)
            }
        }
    }

    fn answer(status: u16, body: &str) -> TargetAnswer {
        TargetAnswer {
            status,
            body: body.as_bytes().to_vec(),
        }
    }

    fn request() -> OutboundRequest {
        OutboundRequest {
            method: GatewayMethod::Get,
            url: "https://93.184.216.34/balance".to_owned(),
            headers: Vec::new(),
            body: None,
        }
    }

    #[tokio::test]
    async fn an_idempotent_endpoint_is_retried_at_most_twice_and_another_is_tried_once() {
        let flaky = Scripted::new(
            vec![
                Ok(answer(503, "")),
                Err("reset".to_owned()),
                Ok(answer(200, r#"{"points": 9}"#)),
            ],
            Duration::ZERO,
        );
        let outcome = send_within(
            &flaky,
            &endpoint(GatewayMethod::Get, true),
            &request(),
            Duration::from_secs(3),
        )
        .await;
        assert_eq!(
            outcome,
            CallOutcome::Answered {
                status: 200,
                result: values(&json!({ "points": 9 }))
            }
        );
        assert_eq!(flaky.attempts.load(Ordering::SeqCst), 3);

        let down = Scripted::new(vec![Ok(answer(502, "")); 5], Duration::ZERO);
        let outcome = send_within(
            &down,
            &endpoint(GatewayMethod::Get, true),
            &request(),
            Duration::from_secs(3),
        )
        .await;
        assert_eq!(
            outcome,
            CallOutcome::Refused {
                refusal: CallRefusal::TargetError,
                status: Some(502)
            }
        );
        assert_eq!(
            down.attempts.load(Ordering::SeqCst),
            3,
            "the first and two retries"
        );

        let once = Scripted::new(
            vec![Ok(answer(503, "")), Ok(answer(200, "{}"))],
            Duration::ZERO,
        );
        let outcome = send_within(
            &once,
            &endpoint(GatewayMethod::Post, false),
            &request(),
            Duration::from_secs(3),
        )
        .await;
        assert_eq!(
            outcome,
            CallOutcome::Refused {
                refusal: CallRefusal::TargetError,
                status: Some(503)
            }
        );
        assert_eq!(
            once.attempts.load(Ordering::SeqCst),
            1,
            "not idempotent: tried once"
        );

        let refused = Scripted::new(
            vec![Ok(answer(404, "")), Ok(answer(200, "{}"))],
            Duration::ZERO,
        );
        let _ = send_within(
            &refused,
            &endpoint(GatewayMethod::Get, true),
            &request(),
            Duration::from_secs(3),
        )
        .await;
        assert_eq!(
            refused.attempts.load(Ordering::SeqCst),
            1,
            "a 4xx is not retried"
        );
    }

    #[tokio::test]
    async fn the_deadline_bounds_every_attempt_together() {
        let slow = Scripted::new(
            vec![Ok(answer(200, r#"{"points": 1}"#))],
            Duration::from_millis(400),
        );
        let outcome = send_within(
            &slow,
            &endpoint(GatewayMethod::Get, true),
            &request(),
            Duration::from_millis(100),
        )
        .await;
        assert_eq!(
            outcome,
            CallOutcome::Refused {
                refusal: CallRefusal::Timeout,
                status: None
            }
        );

        // Each failing attempt takes 60 ms: within 150 ms there is time for the first and one retry
        // to finish, and the third is cut off by the deadline.
        let failing = Scripted::new(vec![Ok(answer(503, "")); 3], Duration::from_millis(60));
        let outcome = send_within(
            &failing,
            &endpoint(GatewayMethod::Get, true),
            &request(),
            Duration::from_millis(150),
        )
        .await;
        assert_eq!(
            outcome,
            CallOutcome::Refused {
                refusal: CallRefusal::Timeout,
                status: None
            }
        );
    }
}
