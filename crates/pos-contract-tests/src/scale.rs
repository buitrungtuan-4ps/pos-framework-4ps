// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `Scale` suite
//! ([ADR-0223](../../../docs/adr/0223-a-weighed-item-is-priced-by-its-weight.md)).
//!
//! The obligations with teeth are [`a_moving_load_has_no_weight`], because a weight taken while
//! the load still moves is a weight nobody sold, and [`an_absent_scale_is_unavailable`], because
//! a scale that answers zero when it is unplugged prices a line at nothing.

use pos_ports::PortName;
use pos_ports::scale::{Scale, ScaleReading};
use pos_proto::ErrorStatus;

use crate::harness::ScaleHarness;
use crate::{CaseFailure, Obligation};

/// Emits every `Scale` case as a `#[test]`.
#[macro_export]
macro_rules! scale_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_SCALE,
            module = scale,
            cases = [
                an_empty_scale_reads_zero,
                a_settled_load_is_its_weight,
                a_moving_load_has_no_weight,
                too_much_or_below_zero_is_said_so,
                an_absent_scale_is_unavailable,
            ]
        }
    };
}

/// A fresh scale, with nothing on it, reads a stable zero.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn an_empty_scale_reads_zero<H: ScaleHarness>(harness: &H) -> Result<(), CaseFailure> {
    let scale = harness.fresh().await?;
    Obligation::new(PortName::Scale, "a settled load is its weight").require_eq(
        &scale.read().await?,
        &ScaleReading::Stable { net_grams: 0 },
        "nothing on the scale is a stable zero",
    )
}

/// A settled load reads its weight in grams, and the same weight again while it rests.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn a_settled_load_is_its_weight<H: ScaleHarness>(harness: &H) -> Result<(), CaseFailure> {
    let scale = harness.fresh().await?;
    harness.settle(&scale, 375).await?;
    let obligation = Obligation::new(PortName::Scale, "a settled load is its weight");
    let settled = ScaleReading::Stable { net_grams: 375 };
    obligation.require_eq(
        &scale.read().await?,
        &settled,
        "the reading is the load's grams",
    )?;
    obligation.require_eq(
        &scale.read().await?,
        &settled,
        "a resting load reads the same again, never rounded or edited",
    )
}

/// A load that moves has no weight, even one that had settled before.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn a_moving_load_has_no_weight<H: ScaleHarness>(harness: &H) -> Result<(), CaseFailure> {
    let scale = harness.fresh().await?;
    harness.settle(&scale, 500).await?;
    harness.disturb(&scale).await?;
    Obligation::new(PortName::Scale, "a moving load has no weight").require_eq(
        &scale.read().await?,
        &ScaleReading::Unstable,
        "a moving load is unstable, never the weight it had before",
    )
}

/// Over the scale's capacity, and below zero, are each said so.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn too_much_or_below_zero_is_said_so<H: ScaleHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let obligation = Obligation::new(PortName::Scale, "too much or below zero is said so");
    let scale = harness.fresh().await?;
    harness.overload(&scale).await?;
    obligation.require_eq(
        &scale.read().await?,
        &ScaleReading::Overloaded,
        "a load over capacity is overloaded, never the capacity",
    )?;
    let scale = harness.fresh().await?;
    harness.below_zero(&scale).await?;
    obligation.require_eq(
        &scale.read().await?,
        &ScaleReading::UnderZero,
        "a reading below zero is under zero, never zero",
    )
}

/// An absent scale is `UNAVAILABLE`, never a weight.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn an_absent_scale_is_unavailable<H: ScaleHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let scale = harness.fresh().await?;
    harness.settle(&scale, 250).await?;
    harness.unplug(&scale).await?;
    Obligation::new(PortName::Scale, "an absent scale is an error").require_error(
        scale.read().await,
        ErrorStatus::Unavailable,
        "an unplugged scale is unavailable, so no line is priced at nothing",
    )
}
