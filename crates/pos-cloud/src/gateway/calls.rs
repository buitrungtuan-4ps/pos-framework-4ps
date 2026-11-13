// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The gateway's idempotency record
//! ([ADR-0198](../../../../docs/adr/0198-a-store-reaches-the-internet-only-through-the-cloud.md)
//! decision 6): a store's idempotency key repeated within 24 hours gets the first answer back, so a
//! till retried by a person does not act twice.
//!
//! Only an answer the target gave is kept: a result, a target's error status, or an answer that did
//! not hold the declared fields. A call that never reached the target, or whose answer did not come
//! back in time, is not kept, so a retry tries again and carries the same `Idempotency-Key` for the
//! target to recognise.
//!
//! A kept answer holds the declared response fields, so it is kept 24 hours and no longer: the store
//! forgets a key once it is older, and a recall never answers one that is.

use core::future::Future;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use pos_proto::ids::{StoreId, TenantId};
use pos_proto::time::Timestamp;

use super::call::CallRefusal;

/// How long an answer is kept, in milliseconds: 24 hours (decision 6).
pub const KEPT_FOR_MS: i64 = 24 * 60 * 60 * 1_000;

/// What the store-facing route answers for one call, and what is kept for its key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct GatewayCallAnswer {
    /// The call's id, a ULID: what the logs name it by. A replayed answer keeps the first call's.
    pub call_id: String,
    /// `GATEWAY_OUTCOME_ANSWERED` with a `result`, or `GATEWAY_OUTCOME_REFUSED` with a `refusal`.
    pub outcome: GatewayOutcome,
    /// Why no result, when refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub refusal: Option<CallRefusal>,
    /// The target's HTTP status, when it answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub status: Option<u16>,
    /// The endpoint's declared response fields, by name; empty when refused.
    #[serde(default)]
    pub result: Map<String, Value>,
}

/// Whether a call answered a result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub enum GatewayOutcome {
    /// The target answered, and `result` holds its declared fields.
    #[serde(rename = "GATEWAY_OUTCOME_ANSWERED")]
    Answered,
    /// No result: `refusal` says why.
    #[serde(rename = "GATEWAY_OUTCOME_REFUSED")]
    Refused,
}

impl GatewayCallAnswer {
    /// Whether the target gave this answer, so it is kept for its key: a result, or a refusal the
    /// target's own answer caused.
    #[must_use]
    pub fn from_target(&self) -> bool {
        match self.refusal {
            None => true,
            Some(refusal) => matches!(
                refusal,
                CallRefusal::TargetError | CallRefusal::MalformedAnswer
            ),
        }
    }
}

/// One kept call: the endpoint it was for, and its answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeptCall {
    /// The endpoint the key was first used for. The same key for another endpoint is refused.
    pub gateway_endpoint_id: String,
    /// The answer.
    pub answer: GatewayCallAnswer,
}

/// Keeps and recalls answers by `(tenant, store, idempotency key)`.
pub trait GatewayCallStore {
    /// The call kept for `idempotency_key` at `store`, unless it is older than [`KEPT_FOR_MS`] at
    /// `now`.
    fn recall(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
        idempotency_key: &str,
        now: Timestamp,
    ) -> impl Future<Output = Result<Option<KeptCall>, GatewayCallStoreError>> + Send;

    /// Keeps `call` for `idempotency_key` at `now`, replacing one kept longer than [`KEPT_FOR_MS`],
    /// and forgets the store's keys older than that.
    fn keep(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
        idempotency_key: &str,
        call: &KeptCall,
        now: Timestamp,
    ) -> impl Future<Output = Result<(), GatewayCallStoreError>> + Send;
}

/// A failure of the idempotency record itself. Never carries an answer's content.
#[derive(Debug, thiserror::Error)]
#[error("the gateway call record failed: {0}")]
pub struct GatewayCallStoreError(String);

impl GatewayCallStoreError {
    /// Wraps a message, for the server's log.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[cfg(test)]
mod tests {
    use super::{GatewayCallAnswer, GatewayOutcome};
    use crate::gateway::call::CallRefusal;

    fn refused(refusal: CallRefusal) -> GatewayCallAnswer {
        GatewayCallAnswer {
            call_id: "01J00000000000000000000C01".to_owned(),
            outcome: GatewayOutcome::Refused,
            refusal: Some(refusal),
            status: None,
            result: serde_json::Map::new(),
        }
    }

    #[test]
    fn only_what_the_target_answered_is_kept() {
        assert!(refused(CallRefusal::TargetError).from_target());
        assert!(refused(CallRefusal::MalformedAnswer).from_target());
        assert!(!refused(CallRefusal::Timeout).from_target());
        assert!(!refused(CallRefusal::Unreachable).from_target());
        assert!(!refused(CallRefusal::InvalidValues).from_target());
        let answered = GatewayCallAnswer {
            outcome: GatewayOutcome::Answered,
            refusal: None,
            ..refused(CallRefusal::Timeout)
        };
        assert!(answered.from_target());
        assert_eq!(
            serde_json::to_value(&answered).expect("json")["outcome"],
            "GATEWAY_OUTCOME_ANSWERED"
        );
    }
}
