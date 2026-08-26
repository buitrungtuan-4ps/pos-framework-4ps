// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The optional cloud metrics heartbeat — the observability wiring for the monitoring profile
//! (WS-D / #103, [ADR-0031](../../../docs/adr/0031-cloud-adapter-transports.md)).
//!
//! Telemetry sits **off the sales path** (`docs/architecture.md` §5): the [`metrics-vm`](metrics_vm)
//! sink records into a bounded queue that drops under pressure, so a slow or dead metrics backend
//! never becomes a trading outage. This module is the sparse *producer* that feeds it — a background
//! loop that samples on an interval and records, mirroring the retention and webhook cron shape.
//!
//! Deliberately minimal, and gated off by default. `docs/capacity-and-reliability.md` turns the
//! monitoring profile **off below ~50 stores** in favour of sparse sampling straight into
//! PostgreSQL, so a pilot cell runs with no `[metrics]` section and emits nothing. When the profile
//! is on, the one series here — `pos.cloud.up`, a liveness heartbeat — is what a dashboard alerts on
//! ("the cloud is up and its telemetry is flowing"); richer per-request series layer onto the same
//! producer as the fleet grows past the profile threshold. There is no PII and no floating point: a
//! sample is an `i64` and a unit, and the heartbeat carries no labels at all.

use core::future::Future;
use core::time::Duration;

use pos_ports::metrics_sink::{MetricName, MetricSample, MetricUnit, MetricsSink};
use pos_proto::ClockSource;
use pos_proto::time::Timestamp;

/// The liveness heartbeat series — dotted `snake_case`, per the metrics-name grammar.
const HEARTBEAT_NAME: &str = "pos.cloud.up";

/// Builds the liveness heartbeat sample: `pos.cloud.up = 1` (a count), stamped `at`, no labels.
///
/// One sample with no dimensions, so it adds exactly one series regardless of fleet size — the
/// "is the cloud process alive and flushing telemetry" signal, and nothing that could carry a
/// person or multiply cardinality.
///
/// # Panics
///
/// Never: `pos.cloud.up` is a valid dotted `snake_case` metric name, asserted by a unit test in this
/// module, so the parse cannot fail at runtime.
#[must_use]
pub fn heartbeat_sample(at: Timestamp) -> MetricSample {
    let name = MetricName::parse(HEARTBEAT_NAME).expect("pos.cloud.up is a valid metric name");
    MetricSample::new(name, 1, MetricUnit::Count, at)
}

/// Runs the sparse metrics heartbeat until `shutdown` resolves.
///
/// Records the heartbeat once per `interval`. A record failure is logged and the loop continues —
/// the sink drops under pressure and reports success, so this only ever sees a genuinely gone sink,
/// which the next tick retries. `biased` select drains the shutdown branch first, so a stop is
/// prompt rather than waiting out the interval.
pub async fn run<M, C>(sink: M, clock: C, interval: Duration, shutdown: impl Future<Output = ()>)
where
    M: MetricsSink,
    C: ClockSource,
{
    tokio::pin!(shutdown);
    loop {
        if let Err(error) = sink.record(&[heartbeat_sample(clock.now())]).await {
            tracing::warn!(%error, "metrics heartbeat could not record; the telemetry backend may be down");
        }
        tokio::select! {
            biased;
            () = &mut shutdown => {
                tracing::info!("metrics heartbeat shutting down");
                return;
            }
            () = tokio::time::sleep(interval) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HEARTBEAT_NAME, heartbeat_sample};

    use pos_ports::metrics_sink::{MetricName, MetricUnit};
    use pos_proto::time::Timestamp;

    #[test]
    fn the_heartbeat_name_is_a_valid_metric_name() {
        // Proves the `expect` in `heartbeat_sample` can never fire at runtime.
        assert!(MetricName::parse(HEARTBEAT_NAME).is_ok());
    }

    #[test]
    fn the_heartbeat_is_one_count_with_no_labels() {
        let at = Timestamp::from_milliseconds_since_epoch(1_767_225_600_000).expect("valid");
        let sample = heartbeat_sample(at);
        assert_eq!(sample.value, 1);
        assert_eq!(sample.unit, MetricUnit::Count);
        assert_eq!(sample.at, at);
        assert!(
            sample.labels.is_empty(),
            "the heartbeat carries no labels — no PII, no cardinality"
        );
    }
}
