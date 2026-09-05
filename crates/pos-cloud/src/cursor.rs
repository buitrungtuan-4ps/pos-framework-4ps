// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The production ingest path: a durable NATS cursor driving [`Cloud::ingest`].
//!
//! In production the edge publishes every event to JetStream and the cloud reads it back here
//! (`docs/roadmap.md` P7). The `/internal/ingest` HTTP route is only the reconciliation re-push
//! target; this cursor is the primary feed.
//!
//! The loop is deliberately small because the two hard guarantees live elsewhere: the durable cursor
//! is [`link_nats::NatsConsumer`]'s, and idempotency is [`Cloud::ingest`]'s. What remains here is the
//! **ack policy** — advance the cursor only after ingest has committed, and otherwise return the
//! batch for redelivery. That policy is a pure function (`decide`) so it is tested without a broker;
//! [`pump`] applies it, and [`run`] repeats [`pump`] until shutdown.

use core::future::Future;
use core::time::Duration;

use link_nats::NatsConsumer;

use pos_ports::PortError;
use pos_ports::event_store::EventStore;

use crate::cloud::{Cloud, IngestOutcome};

/// How long [`run`] waits after a transport error before pulling again, so a downed broker is a
/// slow retry rather than a hot loop. Fixed for this slice; exponential backoff is a later
/// refinement.
const BACKOFF: Duration = Duration::from_secs(2);

/// What one [`pump`] cycle achieved.
///
/// `redelivered` means ingest could not store the batch and it was returned to the stream, so
/// nothing was lost — the next pull will bring it back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PumpOutcome {
    /// Events newly stored this cycle.
    pub ingested: u32,
    /// Events the cloud already had (a redelivery), stored again by nobody.
    pub duplicates: u32,
    /// Undecodable messages the cursor terminated this cycle.
    pub poison: u32,
    /// Whether the batch was returned to the stream instead of acknowledged.
    pub redelivered: bool,
}

/// Whether a pulled batch should advance the cursor or be returned for redelivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// Ingest committed: advance the durable cursor past the batch.
    Acknowledge,
    /// Ingest did not commit: return the batch so it is redelivered.
    Redeliver,
}

/// The pure ack decision, factored out so the policy is unit-tested without a broker.
///
/// Any ingest failure — retryable back-pressure or a terminal fault alike — returns the batch rather
/// than dropping it; idempotent ingest by `event_id` makes the redelivery harmless. A terminal fault
/// is a genuine problem, but the right response is still not to lose the events: it is redelivered
/// and logged loudly by [`pump`] so an operator sees it, not swallowed.
const fn decide(result: &Result<IngestOutcome, PortError>) -> Disposition {
    match result {
        Ok(_) => Disposition::Acknowledge,
        Err(_) => Disposition::Redeliver,
    }
}

/// One pull → ingest → acknowledge cycle.
///
/// Pulls the next batch, ingests it in one transaction, and — only if that commits — acknowledges it
/// so the durable cursor advances. Undecodable messages were already terminated by the pull and are
/// reported in [`PumpOutcome::poison`].
///
/// # Errors
///
/// [`PortError`] if the consumer's transport fails (pulling, acknowledging, or returning the batch).
/// An ingest failure is **not** returned here — it is handled by redelivering the batch — so a caller
/// looping on `pump` treats an `Err` as "the broker is unreachable", which is what [`run`] does.
pub async fn pump<S>(consumer: &NatsConsumer, cloud: &Cloud<S>) -> Result<PumpOutcome, PortError>
where
    S: EventStore + Clone + Send + Sync + 'static,
    S::Tx: Send,
{
    let batch = consumer.pull().await?;
    let poison = batch.poison();
    if batch.is_empty() {
        return Ok(PumpOutcome {
            poison,
            ..PumpOutcome::default()
        });
    }

    let result = cloud.ingest(batch.events()).await;
    match decide(&result) {
        Disposition::Acknowledge => {
            let outcome = result?;
            batch.ack().await?;
            Ok(PumpOutcome {
                ingested: outcome.appended,
                duplicates: outcome.duplicates,
                poison,
                redelivered: false,
            })
        }
        Disposition::Redeliver => {
            batch.nak().await?;
            if let Err(error) = &result {
                if error.is_retryable() {
                    tracing::warn!(error = %error, "ingest back-pressure; batch returned for redelivery");
                } else {
                    tracing::error!(error = %error, "ingest terminally failed; batch returned for operator attention");
                }
            }
            Ok(PumpOutcome {
                poison,
                redelivered: true,
                ..PumpOutcome::default()
            })
        }
    }
}

/// Runs the ingest cursor until `shutdown` resolves.
///
/// Repeats [`pump`], logging each cycle's effect. A transport error is not fatal — the loop backs off
/// (`BACKOFF`) and retries, so a broker restart is ridden out rather than crashing the cloud. When
/// `shutdown` resolves, an in-flight pull is cancelled; because nothing is acknowledged until ingest
/// commits, a cancelled cycle simply redelivers.
///
/// # Errors
///
/// Never returns `Err`; the signature keeps `?` ergonomic and leaves room for a future fatal case.
/// The result is `Ok(())` on a clean shutdown.
pub async fn run<S>(
    consumer: std::sync::Arc<NatsConsumer>,
    cloud: Cloud<S>,
    shutdown: impl Future<Output = ()>,
) -> Result<(), PortError>
where
    S: EventStore + Clone + Send + Sync + 'static,
    S::Tx: Send,
{
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => {
                tracing::info!("ingest cursor shutting down");
                return Ok(());
            }
            result = pump(&consumer, &cloud) => match result {
                Ok(outcome) => {
                    if outcome.poison > 0 {
                        tracing::error!(count = outcome.poison, "terminated undecodable messages on the ingest cursor");
                    }
                    if outcome.ingested > 0 || outcome.duplicates > 0 {
                        tracing::debug!(
                            ingested = outcome.ingested,
                            duplicates = outcome.duplicates,
                            "ingested a batch from NATS"
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(error = %error, backoff_secs = BACKOFF.as_secs(), "ingest cursor transport error; backing off");
                    tokio::time::sleep(BACKOFF).await;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Disposition, decide};

    use pos_ports::{PortError, PortName};

    use crate::cloud::IngestOutcome;

    #[test]
    fn a_committed_ingest_advances_the_cursor() {
        let result = Ok(IngestOutcome {
            appended: 3,
            duplicates: 0,
        });
        assert_eq!(decide(&result), Disposition::Acknowledge);
    }

    #[test]
    fn a_retryable_failure_returns_the_batch() {
        let result = Err(PortError::unavailable(
            PortName::EventStore,
            "the store is unreachable",
        ));
        assert!(result.as_ref().is_err_and(PortError::is_retryable));
        assert_eq!(decide(&result), Disposition::Redeliver);
    }

    #[test]
    fn even_a_terminal_failure_returns_the_batch_rather_than_dropping_it() {
        // The point of the policy: a terminal fault is loud, but events are never dropped on the
        // floor — they are redelivered so ingest (or an operator) can try again.
        let result = Err(PortError::internal(PortName::EventStore, "a bug"));
        assert!(!result.as_ref().is_err_and(PortError::is_retryable));
        assert_eq!(decide(&result), Disposition::Redeliver);
    }
}
