// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The [`ErpSink`] adapter over an [`ErpTransport`]
//! ([ADR-0059](../../../docs/adr/0059-erp-adapter.md)).
//!
//! Everything here is pure but for the one `await` on the transport: build the batch body and map the
//! ERP's HTTP status to the right [`PortError`]. The status mapping is the load-bearing part — the
//! port's contract turns on it: an unknown account fails the *whole* batch
//! [`invalid_argument`](PortError::invalid_argument) (half a day's revenue is worse than none), a
//! revision already recorded is [`already_exists`](PortError::already_exists) (which a retried nightly
//! job treats as success), and a closed accounting period is
//! [`failed_precondition`](PortError::failed_precondition) (a finance conversation, not a retry).
//!
//! # A posting is keyed by the trading day, never the calendar day
//!
//! The batch carries [`ErpBatch::business_date`](pos_ports::erp::ErpBatch) — a bar closing at 02:00
//! posts those sales to the day it opened — and the ERP is expected to supersede an earlier revision
//! of the same `(store, business_date)` rather than accumulate. The idempotency key
//! ([`ErpBatch::idempotency_key`](pos_ports::erp::ErpBatch::idempotency_key)) is spelled out by the
//! port so three adapters cannot invent three meanings for "post this day again".

use pos_ports::erp::{ErpBatch, ErpLine, ErpPostingRef, ErpSink};
use pos_ports::{PortError, PortName};
use pos_proto::{BusinessDate, StoreId};

use crate::wire::{ErpTransport, HttpResponse, Method};

/// The port this adapter serves, for every [`PortError`] it raises.
const PORT: PortName = PortName::ErpSink;

/// The postings collection.
const POSTINGS_PATH: &str = "/v1/erp/postings";

/// The SAP [`ErpSink`] adapter: post a day's revenue and consumption, and read back what posted, over
/// an [`ErpTransport`].
#[derive(Debug, Clone)]
pub struct HttpSapErp<T> {
    transport: T,
}

impl<T> HttpSapErp<T> {
    /// Wraps a transport as a SAP posting channel.
    #[must_use]
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }
}

/// One posting line on the wire.
///
/// `kind` distinguishes revenue from tax (both carry money); `amount_minor`/`currency` ride for those
/// two, and `quantity_milli` for consumption — the ERP values consumption, this framework does not.
#[derive(serde::Serialize)]
struct LineBody {
    kind: &'static str,
    account_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    amount_minor: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    currency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quantity_milli: Option<i64>,
}

impl LineBody {
    fn of(line: &ErpLine) -> Self {
        let amount = line.amount();
        Self {
            kind: line.kind_wire(),
            account_code: line.account_code().as_str().to_owned(),
            amount_minor: amount.map(|money| money.amount_minor),
            currency: amount.map(|money| money.currency_code.as_str().to_owned()),
            quantity_milli: line.quantity().map(pos_proto::Quantity::as_milli),
        }
    }
}

/// A whole day's posting, keyed for supersession by `(store_id, business_date)` with a rising
/// `revision`.
#[derive(serde::Serialize)]
struct BatchBody {
    idempotency_key: String,
    store_id: String,
    business_date: String,
    revision: u32,
    lines: Vec<LineBody>,
}

/// The ERP's receipt for a posting.
#[derive(serde::Deserialize)]
struct PostingBody {
    document_ref: String,
    revision: u32,
}

impl<T: ErpTransport> ErpSink for HttpSapErp<T> {
    fn erp_name(&self) -> &'static str {
        "sap"
    }

    async fn post(&self, batch: &ErpBatch) -> Result<ErpPostingRef, PortError> {
        let body = serde_json::to_vec(&BatchBody {
            idempotency_key: batch.idempotency_key(),
            store_id: batch.store_id.to_string(),
            business_date: batch.business_date.to_string(),
            revision: batch.revision,
            lines: batch.lines.iter().map(LineBody::of).collect(),
        })
        .map_err(|error| {
            PortError::internal(PORT, format!("encoding the posting failed: {error}"))
        })?;
        let response = self
            .transport
            .request(Method::Post, POSTINGS_PATH, Some(body))
            .await
            .map_err(|error| PortError::unavailable(PORT, error.to_string()))?;
        parse_post(&response)
    }

    async fn posted(
        &self,
        store_id: StoreId,
        business_date: BusinessDate,
    ) -> Result<Option<ErpPostingRef>, PortError> {
        // Both keys are URL-safe: a store id is a ULID and a business date is `YYYY-MM-DD`.
        let path = format!("{POSTINGS_PATH}?store_id={store_id}&business_date={business_date}");
        let response = self
            .transport
            .request(Method::Get, &path, None)
            .await
            .map_err(|error| PortError::unavailable(PORT, error.to_string()))?;
        parse_posted(&response)
    }
}

/// Maps a post response to a posting reference, or the right refusal.
fn parse_post(response: &HttpResponse) -> Result<ErpPostingRef, PortError> {
    match response.status {
        200..=299 => Ok(to_posting(&decode(response)?)),
        400 => Err(PortError::invalid_argument(
            PORT,
            "the ERP does not know an account code in this batch",
        )),
        409 => Err(PortError::already_exists(
            PORT,
            "a revision of this day at least this high has already posted",
        )),
        423 => Err(PortError::failed_precondition(
            PORT,
            "the accounting period is closed",
        )),
        other => Err(PortError::unavailable(
            PORT,
            format!("the ERP returned HTTP {other} for a posting"),
        )),
    }
}

/// Maps a look-up response to what posted, or the right absence.
///
/// `2xx` is a posting; `404` is a day nothing was posted for — [`None`], not an error, because that is
/// exactly what the nightly job checks before deciding whether to post; anything else is retryable.
fn parse_posted(response: &HttpResponse) -> Result<Option<ErpPostingRef>, PortError> {
    match response.status {
        200..=299 => Ok(Some(to_posting(&decode(response)?))),
        404 => Ok(None),
        other => Err(PortError::unavailable(
            PORT,
            format!("the ERP returned HTTP {other} for a look-up"),
        )),
    }
}

/// Deserialises a posting body, treating a body that does not parse as the ERP breaking its own
/// contract ([`internal`](PortError::internal)).
fn decode(response: &HttpResponse) -> Result<PostingBody, PortError> {
    serde_json::from_slice(&response.body).map_err(|error| {
        PortError::internal(
            PORT,
            format!("the ERP's posting response did not parse: {error}"),
        )
    })
}

/// Turns a posting body into an [`ErpPostingRef`].
fn to_posting(body: &PostingBody) -> ErpPostingRef {
    ErpPostingRef {
        document_ref: body.document_ref.clone().into(),
        revision: body.revision,
    }
}

#[cfg(test)]
mod tests {
    use super::{HttpResponse, parse_post, parse_posted};

    fn posting(revision: u32) -> Vec<u8> {
        format!(r#"{{"document_ref":"DOC-{revision}","revision":{revision}}}"#).into_bytes()
    }

    fn response(status: u16, body: Vec<u8>) -> HttpResponse {
        HttpResponse { status, body }
    }

    #[test]
    fn a_posted_day_parses_its_document_and_revision() {
        let posting = parse_post(&response(201, posting(0))).expect("a posting");
        assert_eq!(&*posting.document_ref, "DOC-0");
        assert_eq!(posting.revision, 0);
    }

    #[test]
    fn an_unknown_account_fails_the_whole_batch_invalid_argument() {
        // Half a day's revenue in an accounting period is worse than none, because none is visibly
        // missing and half is not.
        let error = parse_post(&response(400, b"unknown account".to_vec())).expect_err("rejected");
        assert_eq!(error.status(), pos_proto::ErrorStatus::InvalidArgument);
    }

    #[test]
    fn an_already_recorded_revision_is_already_exists() {
        // Which a retried nightly job treats as success rather than a failure.
        let error =
            parse_post(&response(409, b"already posted".to_vec())).expect_err("already posted");
        assert_eq!(error.status(), pos_proto::ErrorStatus::AlreadyExists);
    }

    #[test]
    fn a_closed_period_is_failed_precondition() {
        let error = parse_post(&response(423, b"period closed".to_vec())).expect_err("closed");
        assert_eq!(error.status(), pos_proto::ErrorStatus::FailedPrecondition);
    }

    #[test]
    fn a_503_post_is_unavailable() {
        let error = parse_post(&response(503, b"down".to_vec())).expect_err("down");
        assert_eq!(error.status(), pos_proto::ErrorStatus::Unavailable);
    }

    #[test]
    fn a_look_up_of_an_unposted_day_is_none_not_an_error() {
        let looked_up = parse_posted(&response(404, b"nothing".to_vec())).expect("a clean absence");
        assert!(looked_up.is_none());
    }

    #[test]
    fn a_look_up_of_a_posted_day_returns_it() {
        let looked_up = parse_posted(&response(200, posting(2))).expect("a posting");
        assert_eq!(looked_up.expect("some").revision, 2);
    }

    #[test]
    fn a_502_look_up_is_unavailable() {
        let error = parse_posted(&response(502, b"bad gateway".to_vec())).expect_err("bad gateway");
        assert_eq!(error.status(), pos_proto::ErrorStatus::Unavailable);
    }

    #[test]
    fn a_body_that_does_not_parse_is_an_internal_contract_breach() {
        let error = parse_post(&response(200, b"not json".to_vec())).expect_err("garbage body");
        assert_eq!(error.status(), pos_proto::ErrorStatus::Internal);
    }
}
