// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Draining the store's outbox to the cloud
//! ([ADR-0087](../../../docs/adr/0087-edge-relay-and-event-publish.md)).
//!
//! Every event the store commits lands in its **outbox** in commit order
//! ([ADR-0015](../../../docs/adr/0015-sqlite-access.md)). This loop is the other end of that
//! table: it publishes batches over the [`MessageLink`] port and acknowledges exactly what the far
//! side accepted. The cloud's consumer has been running since P7; until this slice there was no
//! publisher, so a committed sale never left the box.
//!
//! # Why commit → publish → acknowledge, in that order
//!
//! There is no transaction spanning NATS and SQLite, and pretending otherwise would either lose
//! events on a crash or publish them twice ([`pos_ports::message_link`] is explicit about this). So
//! delivery is **at-least-once**: an event is acknowledged only once the stream has it, a crash
//! anywhere in the sequence replays the tail, and the cloud's ingest is idempotent by event id. The
//! outbox is what makes that safe, and what lets the counter keep selling while the cloud is
//! unreachable — the table simply grows.
//!
//! # Acceptance is a prefix, not a verdict
//!
//! [`PublishOutcome::accepted`] is a **count from the start of the batch**. A batch of fifty
//! accepted as thirty means thirty are durable and twenty are not; this loop acknowledges through
//! the thirtieth record's position and re-sends the rest on the next pass. Acknowledging the whole
//! batch would silently drop the tail, and re-sending the whole batch would make a link that is 60%
//! healthy behave like one that is 0% healthy.

use core::future::Future;
use core::num::NonZeroU32;
use core::time::Duration;
use std::sync::Arc;

use pos_ports::event_store::{EventStore, OutboxPosition, OutboxRecord};
use pos_ports::message_link::MessageLink;
use pos_proto::protocol::{Hello, HelloOutcome};
use pos_proto::text::ReleaseTag;

use crate::app::Edge;

/// How long to wait after draining everything before looking again. The outbox is local, so this is
/// a cheap query; the interval keeps an idle store from spinning.
const IDLE_INTERVAL: Duration = Duration::from_secs(5);

/// How long to wait after a failure — the link is down, the stream is full, or the store could not
/// be read. Nothing is lost meanwhile: the outbox holds, and the counter keeps trading (ADR-0001).
const RETRY_BACKOFF: Duration = Duration::from_secs(15);

/// How long to wait after a refused handshake. The cloud speaks no version this build does, which a
/// retry in seconds will not fix; back off far enough to be visible in a log rather than to drown it
/// ([`pos_proto::protocol`] is explicit that a refusal must not become a tight loop).
const REFUSED_BACKOFF: Duration = Duration::from_secs(300);

/// The position to acknowledge through for an accepted prefix, and how many records that is.
///
/// `accepted` counts from the **start** of the batch, so the last durable record is at index
/// `accepted - 1`. `None` means nothing landed and nothing may be acknowledged — the whole batch is
/// re-sent. An `accepted` larger than the batch is clamped to the batch: a far side claiming more
/// than it was sent must never advance the high-water mark past what this loop actually published.
///
/// Takes the batch's positions rather than its records, so the arithmetic that would otherwise
/// silently drop a tail is a pure function checked in the fast gate.
fn acknowledge_through(
    positions: &[OutboxPosition],
    accepted: u32,
) -> Option<(OutboxPosition, usize)> {
    let accepted = usize::try_from(accepted)
        .unwrap_or(usize::MAX)
        .min(positions.len());
    let last = positions
        .get(..accepted)
        .and_then(<[OutboxPosition]>::last)?;
    Some((*last, accepted))
}

/// Publishes the store's committed events to the cloud, and acknowledges what lands.
///
/// Generic over the store `S` and the link `L` — static dispatch, no `dyn`
/// ([ADR-0013](../../../docs/adr/0013-async-strategy.md)) — so the loop is driven in tests by the
/// in-memory fakes and in the field by `store-sqlite` and `link-nats`.
#[derive(Debug)]
pub struct EventPublisher<S, L> {
    edge: Arc<Edge<S>>,
    link: L,
    release: ReleaseTag,
}

impl<S, L> EventPublisher<S, L>
where
    S: EventStore,
    L: MessageLink,
{
    /// Builds a publisher over the `edge`'s own log and `link`, advertising `release` at handshake.
    ///
    /// It takes the [`Edge`] rather than a bare store because the `Edge` owns the log
    /// ([`Edge::store`]); the publisher only ever reads the outbox and acknowledges it.
    pub const fn new(edge: Arc<Edge<S>>, link: L, release: ReleaseTag) -> Self {
        Self {
            edge,
            link,
            release,
        }
    }

    /// The batch size the link will take: its own declared maximum
    /// ([`MessageLink::max_batch_size`]), so a batch is never refused for being too large.
    fn batch_size(&self) -> NonZeroU32 {
        self.link.max_batch_size()
    }

    /// Negotiates the protocol version for this connection.
    ///
    /// Returns `false` when the cloud refuses — no version in common — which is a condition to log
    /// and back off from, never to retry tightly and never to stop trading over.
    async fn handshake(&self) -> bool {
        let hello = Hello::current(self.edge.store_id(), self.release.clone());
        match self.link.handshake(&hello).await {
            Ok(HelloOutcome::Accepted { protocol_version }) => {
                tracing::info!(protocol_version, "event publish: link handshake accepted");
                true
            }
            Ok(HelloOutcome::Refused {
                minimum_supported,
                maximum_supported,
            }) => {
                tracing::error!(
                    minimum_supported,
                    maximum_supported,
                    ours = pos_proto::PROTOCOL_VERSION,
                    "event publish: the cloud speaks no protocol version this build does; \
                     events stay in the outbox and the store keeps trading"
                );
                false
            }
            Err(error) => {
                tracing::warn!(%error, "event publish: the link handshake failed");
                false
            }
        }
    }

    /// Publishes one batch and acknowledges the accepted prefix.
    ///
    /// Returns how many records were acknowledged — `0` when there was nothing to send, or when the
    /// link accepted nothing; either way the loop idles rather than spinning.
    ///
    /// # Errors
    ///
    /// [`pos_ports::PortError`] if the outbox could not be read, the publish failed, or the
    /// acknowledgement did not land. Every one of those leaves the outbox intact: the same records
    /// are re-read next pass.
    async fn drain_once(&self) -> Result<usize, pos_ports::PortError> {
        let store_id = self.edge.store_id();
        let batch: Vec<OutboxRecord> = self
            .edge
            .store()
            .outbox_batch(store_id, OutboxPosition::START, self.batch_size())
            .await?;
        if batch.is_empty() {
            return Ok(0);
        }

        let envelopes: Vec<_> = batch.iter().map(|record| record.envelope.clone()).collect();
        let outcome = self.link.publish(&envelopes).await?;

        let positions: Vec<OutboxPosition> = batch.iter().map(|record| record.position).collect();
        let Some((through, accepted)) = acknowledge_through(&positions, outcome.accepted) else {
            tracing::warn!(
                batch = batch.len(),
                "event publish: the link accepted nothing; the outbox holds and will retry"
            );
            return Ok(0);
        };
        self.edge
            .store()
            .acknowledge_outbox(store_id, through)
            .await?;
        Ok(accepted)
    }

    /// Runs until `shutdown` resolves: handshake, then drain the outbox as fast as it fills.
    ///
    /// A full batch means there is probably more waiting, so the loop goes straight round again; an
    /// empty one idles. Every failure is logged and backed off from — the store keeps selling either
    /// way, and the outbox is what makes that safe.
    pub async fn run<F>(self, shutdown: F)
    where
        F: Future<Output = ()> + Send,
    {
        let batch_size = usize::try_from(self.batch_size().get()).unwrap_or(usize::MAX);
        tokio::pin!(shutdown);

        // The handshake is per connection, not per batch; a refusal is re-attempted only after the
        // long backoff, so a version mismatch does not become a hot loop.
        loop {
            if self.handshake().await {
                break;
            }
            tokio::select! {
                () = &mut shutdown => return,
                () = tokio::time::sleep(REFUSED_BACKOFF) => {}
            }
        }

        loop {
            let wait = match self.drain_once().await {
                Ok(0) => IDLE_INTERVAL,
                Ok(published) => {
                    tracing::debug!(published, "event publish: batch acknowledged");
                    // A full batch almost certainly means more is waiting; go straight round.
                    if published >= batch_size {
                        continue;
                    }
                    IDLE_INTERVAL
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "event publish: the batch did not land; the outbox holds and the store keeps trading"
                    );
                    RETRY_BACKOFF
                }
            };
            tokio::select! {
                () = &mut shutdown => return,
                () = tokio::time::sleep(wait) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{OutboxPosition, acknowledge_through};

    #[test]
    fn a_whole_batch_acknowledges_through_its_last_record() {
        let batch = [
            OutboxPosition::new(10),
            OutboxPosition::new(11),
            OutboxPosition::new(12),
        ];
        let (through, count) = acknowledge_through(&batch, 3).expect("a full accept acknowledges");
        assert_eq!(through, OutboxPosition::new(12));
        assert_eq!(count, 3);
    }

    #[test]
    fn a_partial_accept_acknowledges_only_the_prefix() {
        // The bug this guards: acknowledging the whole batch on a partial accept silently drops the
        // tail. Two of three accepted must advance the mark to the *second* record, leaving the
        // third to be re-sent (ADR-0087).
        let batch = [
            OutboxPosition::new(10),
            OutboxPosition::new(11),
            OutboxPosition::new(12),
        ];
        let (through, count) =
            acknowledge_through(&batch, 2).expect("a partial accept acknowledges");
        assert_eq!(through, OutboxPosition::new(11));
        assert_eq!(count, 2);
    }

    #[test]
    fn accepting_nothing_acknowledges_nothing() {
        let batch = [OutboxPosition::new(10), OutboxPosition::new(11)];
        assert!(acknowledge_through(&batch, 0).is_none());
    }

    #[test]
    fn an_over_reported_accept_cannot_advance_past_what_was_sent() {
        // A far side claiming more than it was given must not move the high-water mark beyond the
        // batch — that would acknowledge events the loop has not published yet.
        let batch = [OutboxPosition::new(10), OutboxPosition::new(11)];
        let (through, count) = acknowledge_through(&batch, 99).expect("clamped to the batch");
        assert_eq!(through, OutboxPosition::new(11));
        assert_eq!(count, 2);
    }

    #[test]
    fn an_empty_batch_acknowledges_nothing() {
        assert!(acknowledge_through(&[], 5).is_none());
    }
}
