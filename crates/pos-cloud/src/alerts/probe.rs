// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The fleet stream's ceiling and its fill
//! ([ADR-0087](../../../docs/adr/0087-edge-relay-and-event-publish.md) Amendment 2, roadmap
//! **A·P4 O4**).
//!
//! [ADR-0073](../../../docs/adr/0073-alerting.md) shipped `AlertKind::JetstreamCapacity` and
//! [`evaluate`](super::eval::evaluate)'s arm for it, and the evaluator has fed them `None` ever
//! since — the probe was a flagged follow-up. This is that probe, and it does two jobs in one call
//! because they read the same `stream.info()`:
//!
//!  * **It reconciles the ceiling.** The limits in force are whatever the *first* store to connect
//!    asked for, because the edge's `ensure_stream` is a create-or-get that by design does not
//!    reconcile an existing stream. No edge release can move them; the cloud can, and is the only
//!    party that knows how many stores the estate runs.
//!  * **It reports the fill**, so the 80% alert fires before the ceiling is reached rather than after
//!    — which matters because `discard: new` means a full stream refuses *every* store's publish.
//!
//! A trait so the evaluator runs against a fake in tests and a real JetStream connection in the
//! cloud, exactly as [`FleetStore`](crate::fleet::FleetStore) does.

use core::future::Future;

use pos_ports::message_link::LinkCapacity;

/// Reconciles the fleet stream's limits and reports how full it is.
pub trait StreamCapacityProbe {
    /// Applies the configured ceiling to the stream and returns its fill.
    ///
    /// # Errors
    ///
    /// [`StreamProbeError`] if the stream cannot be read or updated — including the ordinary case of
    /// a deployment whose stores have never connected, so the stream does not exist yet.
    fn reconcile(&self) -> impl Future<Output = Result<LinkCapacity, StreamProbeError>> + Send;
}

/// A failure to read or reconcile the fleet stream.
#[derive(Debug, thiserror::Error)]
#[error("the stream capacity probe failed: {0}")]
pub struct StreamProbeError(String);

impl StreamProbeError {
    /// Wraps a reason (for the server's log — a stream's fill is a count, not a person).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// The shipped probe: the cloud's own durable cursor, which already holds a JetStream connection to
/// the fleet stream, plus the ceiling from `cloud.toml`'s `[nats]` section.
///
/// Holding the limits here rather than passing them per call keeps the trait argument-free, so a test
/// fake has nothing to get wrong about which numbers were meant.
#[derive(Debug)]
pub struct CursorProbe {
    cursor: std::sync::Arc<link_nats::NatsConsumer>,
    max_messages: i64,
    max_bytes: i64,
}

impl CursorProbe {
    /// Binds the probe to the cursor's connection and the operator's ceiling.
    #[must_use]
    pub const fn new(
        cursor: std::sync::Arc<link_nats::NatsConsumer>,
        max_messages: i64,
        max_bytes: i64,
    ) -> Self {
        Self {
            cursor,
            max_messages,
            max_bytes,
        }
    }
}

impl StreamCapacityProbe for CursorProbe {
    async fn reconcile(&self) -> Result<LinkCapacity, StreamProbeError> {
        self.cursor
            .reconcile_capacity(self.max_messages, self.max_bytes)
            .await
            .map_err(|error| StreamProbeError::new(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::{StreamCapacityProbe, StreamProbeError};
    use crate::alerts::eval::AlertThresholds;
    use crate::alerts::model::AlertKind;
    use pos_ports::message_link::LinkCapacity;

    /// A probe that answers with whatever the test set — including a failure, which is the ordinary
    /// state of a deployment whose stores have never connected.
    struct Fixed(Result<LinkCapacity, String>);

    impl StreamCapacityProbe for Fixed {
        async fn reconcile(&self) -> Result<LinkCapacity, StreamProbeError> {
            self.0
                .as_ref()
                .copied()
                .map_err(|error| StreamProbeError::new(error.clone()))
        }
    }

    #[tokio::test]
    async fn a_reading_past_the_threshold_is_what_raises_the_alert() {
        // The condition and its threshold shipped with ADR-0073 and have been fed `None` ever since.
        // This is the half that was missing: a real reading reaching `evaluate`.
        let probe = Fixed(Ok(LinkCapacity {
            messages: 850_000,
            message_limit: Some(1_000_000),
            bytes: 0,
            byte_limit: None,
        }));
        let capacity = probe.reconcile().await.expect("the probe answers");
        let thresholds = AlertThresholds::default();
        let firing = crate::alerts::eval::evaluate(
            pos_proto::time::Timestamp::from_milliseconds_since_epoch(1_777_000_000_000)
                .expect("a valid timestamp"),
            &thresholds,
            &[],
            &[],
            Some(&capacity),
        );
        assert!(
            firing
                .iter()
                .any(|alert| alert.kind == AlertKind::JetstreamCapacity),
            "85% of the message limit is past the 80% threshold"
        );
    }

    #[tokio::test]
    async fn an_unreachable_stream_is_a_missing_reading_and_not_a_failed_pass() {
        // The commonest case is entirely ordinary: the edge creates the stream, so a cloud whose
        // stores have never connected has nothing to read. It must not stop the *other* conditions
        // firing — a broker that is down is exactly when an operator wants the fleet view.
        let probe = Fixed(Err("stream not found".to_owned()));
        let error = probe.reconcile().await.expect_err("the probe fails");
        assert!(error.to_string().contains("stream not found"));
    }
}
