// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A store's answers to its delivery vendors, sent through the cloud
//! ([ADR-0215](../../../docs/adr/0215-a-delivery-order-names-its-vendor.md) decisions 4 and 5).
//!
//! The store accepts an order from a vendor in the `vendors` node when it takes it: at once, or when
//! a member of staff confirms it where that vendor's orders wait for one. It rejects one staff
//! refuse, citing the reason, and says an order is ready once the kitchen has made every line. Each
//! answer names the vendor by its opaque id and the order by the id the store gave it, so the cloud
//! finds the vendor's reference; nothing here knows one vendor from another.
//!
//! An answer never holds up the till. It goes on a bounded queue ([`MAX_WAITING`]) that one task
//! drains to `POST /sync/stores/{store_id}/delivery/answers`, retrying a cloud or vendor it could
//! not reach a few times ([`ATTEMPTS`]); the cloud makes a retry one answer to the vendor by its
//! idempotency key. A full queue, a store with no cloud, or a restart loses the answer, which is
//! logged by vendor and order id. Nothing personal is sent or logged.

use core::future::Future;
use core::time::Duration;

use serde::Serialize;
use tokio::sync::mpsc;

use pos_proto::ids::{OrderId, ReasonCodeId};
use pos_proto::vendors::PublishedVendor;

use crate::relay_client::RelayTransportError;

/// The most answers waiting to be sent.
pub const MAX_WAITING: usize = 256;
/// How many times one answer is tried.
pub const ATTEMPTS: u32 = 4;
/// The wait before the second try; it doubles after each.
const FIRST_RETRY: Duration = Duration::from_secs(2);

/// What a store tells a vendor about one order. The cloud's generic answer codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// The store takes the order and will have it ready in the vendor's `prep_minutes`.
    Accept,
    /// The store will not make it, for this reason.
    Reject(ReasonCodeId),
    /// The kitchen has made it.
    Ready,
}

/// One answer, as the store posts it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AnswerBody {
    vendor_id: String,
    order_id: String,
    answer: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    prep_minutes: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason_code_id: Option<String>,
}

impl AnswerBody {
    /// `answer` about `order_id` to `vendor`, with the vendor's minutes for an accept.
    #[must_use]
    pub fn new(vendor: &PublishedVendor, order_id: OrderId, answer: Answer) -> Self {
        let (word, prep_minutes, reason_code_id) = match answer {
            Answer::Accept => ("DELIVERY_ANSWER_ACCEPT", Some(vendor.prep_minutes), None),
            Answer::Reject(reason) => ("DELIVERY_ANSWER_REJECT", None, Some(reason.to_string())),
            Answer::Ready => ("DELIVERY_ANSWER_READY", None, None),
        };
        Self {
            vendor_id: vendor.vendor_id.to_string(),
            order_id: order_id.to_string(),
            answer: word,
            prep_minutes,
            reason_code_id,
        }
    }

    /// The answer's code, for the log.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.answer
    }
}

/// How the cloud answered one post.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Posted {
    /// The vendor took it.
    Taken,
    /// The cloud or the vendor could not take it now; worth another try.
    TryAgain,
    /// The cloud refused it for good (no such vendor, order or answer); not worth another try.
    Refused,
}

impl Posted {
    /// What an HTTP status from the answers route means.
    #[must_use]
    pub const fn of_status(status: u16) -> Self {
        match status {
            200..=299 => Self::Taken,
            429 | 500..=599 => Self::TryAgain,
            _ => Self::Refused,
        }
    }
}

/// Where answers are posted: the store's own cloud.
pub trait AnswerTransport: Send + Sync {
    /// Posts one answer.
    ///
    /// # Errors
    ///
    /// [`RelayTransportError`] if the cloud could not be reached.
    fn post(
        &self,
        body: &AnswerBody,
    ) -> impl Future<Output = Result<Posted, RelayTransportError>> + Send;
}

/// The sending end of the queue: where an answer is handed over. Cheap to clone.
#[derive(Clone, Debug)]
pub struct DeliveryAnswers {
    sender: mpsc::Sender<AnswerBody>,
}

/// The receiving end of the queue, drained by [`run`].
#[derive(Debug)]
pub struct AnswerQueue {
    receiver: mpsc::Receiver<AnswerBody>,
}

/// A bounded queue of answers: what hands them over, and what drains them.
#[must_use]
pub fn channel() -> (DeliveryAnswers, AnswerQueue) {
    let (sender, receiver) = mpsc::channel(MAX_WAITING);
    (DeliveryAnswers { sender }, AnswerQueue { receiver })
}

impl DeliveryAnswers {
    /// Hands over `answer` about `order_id` to `vendor`. Never waits: a full or closed queue logs
    /// and drops it.
    pub fn send(&self, vendor: &PublishedVendor, order_id: OrderId, answer: Answer) {
        let body = AnswerBody::new(vendor, order_id, answer);
        let code = body.code();
        if self.sender.try_send(body).is_err() {
            tracing::warn!(
                vendor_id = %vendor.vendor_id,
                %order_id,
                answer = code,
                "a delivery answer could not be queued and is lost"
            );
        }
    }
}

/// Sends every answer handed over until `shutdown` resolves, trying each up to [`ATTEMPTS`] times.
pub async fn run<T, F>(mut queue: AnswerQueue, transport: T, shutdown: F)
where
    T: AnswerTransport,
    F: Future<Output = ()> + Send,
{
    tokio::pin!(shutdown);
    loop {
        let body = tokio::select! {
            () = &mut shutdown => return,
            next = queue.receiver.recv() => match next {
                Some(body) => body,
                None => return,
            },
        };
        let outcome = deliver(&transport, &body, FIRST_RETRY).await;
        let vendor_id = body.vendor_id.as_str();
        let order_id = body.order_id.as_str();
        match outcome {
            Posted::Taken => {
                tracing::info!(
                    vendor_id,
                    order_id,
                    answer = body.code(),
                    "a delivery vendor was answered"
                );
            }
            Posted::TryAgain | Posted::Refused => tracing::warn!(
                vendor_id,
                order_id,
                answer = body.code(),
                outcome = ?outcome,
                "a delivery vendor could not be answered"
            ),
        }
    }
}

/// Posts `body` until it is taken or refused, or [`ATTEMPTS`] are spent, waiting `first_retry` and
/// then twice as long before each further try.
pub async fn deliver<T: AnswerTransport>(
    transport: &T,
    body: &AnswerBody,
    first_retry: Duration,
) -> Posted {
    let mut wait = first_retry;
    let mut last = Posted::TryAgain;
    for attempt in 1..=ATTEMPTS {
        last = transport.post(body).await.unwrap_or(Posted::TryAgain);
        if last != Posted::TryAgain || attempt == ATTEMPTS {
            break;
        }
        tokio::time::sleep(wait).await;
        wait = wait.saturating_mul(2);
    }
    last
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use pos_proto::ids::{OrderId, ReasonCodeId, VendorId};
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;
    use pos_proto::vendors::PublishedVendor;

    use super::{Answer, AnswerBody, AnswerTransport, Posted, deliver};
    use crate::relay_client::RelayTransportError;

    fn vendor() -> PublishedVendor {
        PublishedVendor {
            vendor_id: VendorId::new(Ulid::from_u128(1)),
            display_name: DisplayName::new("Sandbox delivery"),
            staff_confirm: true,
            prep_minutes: 20,
        }
    }

    struct Answers(Mutex<Vec<Posted>>);

    impl AnswerTransport for Answers {
        async fn post(&self, _body: &AnswerBody) -> Result<Posted, RelayTransportError> {
            let mut left = self.0.lock().expect("lock");
            if left.is_empty() {
                Err(RelayTransportError::new("unreachable"))
            } else {
                Ok(left.remove(0))
            }
        }
    }

    #[test]
    fn an_answer_names_the_vendor_and_the_order_and_only_what_its_code_needs() {
        let order = OrderId::new(Ulid::from_u128(2));
        let accept =
            serde_json::to_value(AnswerBody::new(&vendor(), order, Answer::Accept)).expect("json");
        assert_eq!(accept["answer"], "DELIVERY_ANSWER_ACCEPT");
        assert_eq!(accept["prep_minutes"], 20);
        assert_eq!(accept["order_id"], order.to_string());
        assert!(accept.get("reason_code_id").is_none());
        let reason = ReasonCodeId::new(Ulid::from_u128(3));
        let reject =
            serde_json::to_value(AnswerBody::new(&vendor(), order, Answer::Reject(reason)))
                .expect("json");
        assert_eq!(reject["reason_code_id"], reason.to_string());
        assert!(reject.get("prep_minutes").is_none());
        let ready =
            serde_json::to_value(AnswerBody::new(&vendor(), order, Answer::Ready)).expect("json");
        assert_eq!(ready["answer"], "DELIVERY_ANSWER_READY");
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_is_tried_again_only_while_it_may_yet_be_taken() {
        let body = AnswerBody::new(&vendor(), OrderId::new(Ulid::from_u128(2)), Answer::Ready);
        let later = Answers(Mutex::new(vec![Posted::TryAgain, Posted::Taken]));
        assert_eq!(
            deliver(&later, &body, core::time::Duration::from_secs(1)).await,
            Posted::Taken
        );
        let refused = Answers(Mutex::new(vec![Posted::Refused, Posted::Taken]));
        assert_eq!(
            deliver(&refused, &body, core::time::Duration::from_secs(1)).await,
            Posted::Refused,
            "a refusal is not tried again"
        );
        let never = Answers(Mutex::new(Vec::new()));
        assert_eq!(
            deliver(&never, &body, core::time::Duration::from_secs(1)).await,
            Posted::TryAgain,
            "an unreachable cloud gives up after the last attempt"
        );
        assert_eq!(Posted::of_status(204), Posted::Taken);
        assert_eq!(Posted::of_status(503), Posted::TryAgain);
        assert_eq!(Posted::of_status(404), Posted::Refused);
    }
}
