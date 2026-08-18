// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `MetricsSink` suite.
//!
//! The obligation with teeth is [`never_blocks_the_caller_under_pressure`]. Telemetry sits off the
//! sales path by design (`docs/architecture.md` §5), so a saturated metrics backend must degrade
//! to dropped samples, never to a slow sale. An adapter that blocks here turns a monitoring
//! outage into a trading outage.

use pos_ports::PortName;
use pos_ports::metrics_sink::{
    MetricLabel, MetricLabelValue, MetricName, MetricSample, MetricUnit, MetricsSink,
};

use crate::harness::MetricsSinkHarness;
use crate::{CaseFailure, Obligation, fixtures};

/// Emits every `MetricsSink` case as a `#[test]`.
#[macro_export]
macro_rules! metrics_sink_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_METRICS_SINK,
            module = metrics_sink,
            cases = [
                records_a_batch_whole,
                preserves_labels_without_inventing_any,
                accepts_an_empty_batch,
                never_blocks_the_caller_under_pressure,
            ]
        }
    };
}

fn obligation() -> Obligation {
    Obligation::new(
        PortName::MetricsSink,
        "telemetry never fails a caller's real work",
    )
}

/// A sample, or a failure naming the bad literal.
fn sample(name: &str, value: i64) -> Result<MetricSample, CaseFailure> {
    let name = MetricName::parse(name)
        .map_err(|error| CaseFailure::new(format!("fixture metric `{name}`: {error}")))?;
    Ok(MetricSample::new(
        name,
        value,
        MetricUnit::Items,
        fixtures::instant(),
    ))
}

/// Everything in the batch arrives, in order.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn records_a_batch_whole<H: MetricsSinkHarness>(harness: &H) -> Result<(), CaseFailure> {
    let sink = harness.fresh().await?;
    let batch = vec![
        sample("pos.outbox.depth", 7)?,
        sample("pos.print.queue_depth", 2)?,
    ];
    sink.record(&batch).await?;

    let recorded = harness.recorded(&sink).await?;
    let obligation = obligation();
    obligation.require_len(&recorded, 2, "a batch is accepted whole or not at all")?;
    let first = obligation.require_nth(&recorded, 0, "the first sample")?;
    obligation.require_eq(
        &first.value,
        &7,
        "and the values are not rounded or rescaled",
    )?;
    obligation.require_eq(
        &first.unit,
        &MetricUnit::Items,
        "and the unit survives — a threshold set against the wrong unit never fires",
    )
}

/// An adapter passes labels through and adds none.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn preserves_labels_without_inventing_any<H: MetricsSinkHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let sink = harness.fresh().await?;
    let label = MetricLabel::parse("port_name")
        .map_err(|error| CaseFailure::new(format!("fixture label: {error}")))?;
    let value = MetricLabelValue::parse("event_store")
        .map_err(|error| CaseFailure::new(format!("fixture label value: {error}")))?;
    let batch = vec![sample("pos.port.latency", 12)?.with_label(label.clone(), value.clone())];
    sink.record(&batch).await?;

    let recorded = harness.recorded(&sink).await?;
    let obligation = obligation();
    let only = obligation.require_nth(&recorded, 0, "the recorded sample")?;
    obligation.require_eq(
        &only.labels.as_slice(),
        &[(label, value)].as_slice(),
        "labels pass through unchanged and none are added. Cardinality is multiplicative, so an \
         adapter helpfully attaching a hostname multiplies every series in the deployment",
    )
}

/// Recording nothing is not an error.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn accepts_an_empty_batch<H: MetricsSinkHarness>(harness: &H) -> Result<(), CaseFailure> {
    let sink = harness.fresh().await?;
    sink.record(&[]).await?;
    obligation().require_len(
        &harness.recorded(&sink).await?,
        0,
        "an empty batch records nothing and succeeds — a scrape with nothing to report is normal",
    )?;
    Ok(())
}

/// A saturated sink drops or pushes back; it does not block.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn never_blocks_the_caller_under_pressure<H: MetricsSinkHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let sink = harness.fresh().await?;
    harness.saturate(&sink).await?;

    let batch = vec![sample("pos.outbox.depth", 1)?];
    let obligation = obligation();
    match sink.record(&batch).await {
        // Dropping is the preferred behaviour, and reporting success for a dropped sample is
        // correct here: the caller has nothing useful to do about it, and telemetry is sampled.
        Ok(()) => Ok(()),
        Err(error) => obligation.require(
            error.status() == pos_proto::ErrorStatus::ResourceExhausted,
            format!(
                "a saturated sink may report resource_exhausted, but nothing else — anything a \
                 caller might retry or escalate puts a monitoring outage on the sales path. Got \
                 {}",
                pos_proto::wire_enum::WireEnum::as_wire(error.status())
            ),
        ),
    }
}
