// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The order relay's routes, and the recipient a relayed delivery order names, read from the
//! store's own `GET /sync/stores/{store_id}/subjects/{subject_id}`
//! ([ADR-0215](../../../../docs/adr/0215-a-delivery-order-names-its-vendor.md) decision 3). The
//! answer is personal data: it becomes a [`SubjectRecord`] and nothing of it reaches a log.

use std::collections::BTreeMap;

use pos_ports::subject_store::SubjectRecord;
use pos_proto::ids::{StoreId, SubjectId};
use pos_proto::time::Timestamp;
use serde::Deserialize;

use super::CloudHttpClient;
use crate::relay_client::RelayTransportError;

/// The cloud's answer. Mirrors `pos_cloud::connectors::recipient`'s; no `Debug`, so no value can be
/// printed.
#[derive(Deserialize)]
struct RecipientDto {
    collected_time: i64,
    #[serde(default)]
    retain_until_time: Option<i64>,
    fields: BTreeMap<String, String>,
}

/// The pull route for a store. Pure, so the path the transport builds is checked without a socket.
pub(super) fn relay_pull_path(store_id: StoreId) -> String {
    format!("/sync/stores/{store_id}/orders")
}

/// The ack route for one pulled order. Pure, for the same reason.
pub(super) fn relay_ack_path(store_id: StoreId, queued_id: &str) -> String {
    format!("/sync/stores/{store_id}/orders/{queued_id}/ack")
}

/// The read route for one subject. Pure, so the path is checked without a socket.
fn recipient_path(store_id: StoreId, subject_id: SubjectId) -> String {
    format!("/sync/stores/{store_id}/subjects/{subject_id}")
}

/// Reads one recipient: `Some` on `200`, `None` on `404`, an error otherwise.
pub(super) async fn fetch_recipient(
    client: &CloudHttpClient,
    store_id: StoreId,
    subject_id: SubjectId,
) -> Result<Option<SubjectRecord>, RelayTransportError> {
    let (status, body) = client
        .request(
            &hyper::Method::GET,
            &recipient_path(store_id, subject_id),
            None,
            Vec::new(),
        )
        .await
        .map_err(|error| RelayTransportError::new(error.to_string()))?;
    match status {
        200 => record_of(&body).map(Some),
        404 => Ok(None),
        other => Err(RelayTransportError::new(format!(
            "the cloud refused the recipient read with status {other}"
        ))),
    }
}

/// The store's record of an answer: when the cloud kept it and when it expires, as the cloud set.
fn record_of(body: &[u8]) -> Result<SubjectRecord, RelayTransportError> {
    let unreadable = || RelayTransportError::new("the recipient response did not parse");
    let dto: RecipientDto = serde_json::from_slice(body).map_err(|_| unreadable())?;
    let collected_at =
        Timestamp::from_milliseconds_since_epoch(dto.collected_time).map_err(|_| unreadable())?;
    let record = SubjectRecord::new(collected_at, dto.fields);
    Ok(match dto.retain_until_time {
        Some(until) => record.retained_until(
            Timestamp::from_milliseconds_since_epoch(until).map_err(|_| unreadable())?,
        ),
        None => record,
    })
}

#[cfg(test)]
mod tests {
    use pos_proto::ids::{StoreId, SubjectId};
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;

    use super::{recipient_path, record_of};

    #[test]
    fn the_cloud_s_answer_becomes_the_store_s_record_with_its_expiry() {
        let record = record_of(
            br#"{"subject_id":"01J00000000000000000000001","collected_time":1000,"retain_until_time":5000,"fields":{"address":"1 Example Street"}}"#,
        )
        .expect("reads");
        assert_eq!(record.collected_at.as_milliseconds_since_epoch(), 1_000);
        assert_eq!(
            record
                .retain_until
                .map(Timestamp::as_milliseconds_since_epoch),
            Some(5_000)
        );
        assert_eq!(
            record.fields.get("address").map(String::as_str),
            Some("1 Example Street")
        );
        let error = record_of(br#"{"fields":"1 Example Street"}"#).expect_err("refused");
        assert!(
            !error.to_string().contains("Example"),
            "no value in the error"
        );
        assert_eq!(
            recipient_path(
                StoreId::new(Ulid::from_u128(1)),
                SubjectId::new(Ulid::from_u128(2))
            ),
            format!(
                "/sync/stores/{}/subjects/{}",
                StoreId::new(Ulid::from_u128(1)),
                SubjectId::new(Ulid::from_u128(2))
            )
        );
    }
}
