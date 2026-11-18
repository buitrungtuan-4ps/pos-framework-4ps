// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A scale beside the till: one stable net weight, or why there is none
//! ([ADR-0223](../../../docs/adr/0223-a-weighed-item-is-priced-by-its-weight.md) decision 2).
//!
//! The twenty-third port. The edge drives it, because a browser cannot reach a serial or USB scale
//! on every device class. A weighed item is sold by the kilogram and its line counts grams, so a
//! reading is a whole number of grams: no float, and no other unit crosses this boundary. A real
//! scale's protocol is an adapter (phase 8); the shared contract below holds every one of them.
//!
//! # A reading is never edited
//!
//! Where trade law requires a certified scale, the weight sold is the weight the scale reported.
//! The port reports what the scale says and nothing else: it neither rounds, nor takes a tare off
//! (the caller does, from the item's `tare_grams`), nor holds a reading the scale has since
//! withdrawn.

use core::future::Future;

use crate::error::PortError;

/// What a scale answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleReading {
    /// The load has settled: its net weight, in grams.
    Stable {
        /// The net weight the scale reports, in whole grams.
        net_grams: u32,
    },
    /// The load is still moving. No weight may be taken from it.
    Unstable,
    /// The load is above what the scale can weigh.
    Overloaded,
    /// The scale reads below zero, as when a tare was set with a load that was then lifted.
    UnderZero,
}

/// Reads the scale beside a till.
///
/// # Contract
///
/// 1. **A settled load is its weight.** With a load settled on it, [`Self::read`] answers
///    [`ScaleReading::Stable`] with that weight in grams, and the same weight again while it rests.
/// 2. **A moving load has no weight.** While a load moves, the answer is
///    [`ScaleReading::Unstable`], never a number.
/// 3. **Too much, or below zero, is said so.** Over the scale's capacity is
///    [`ScaleReading::Overloaded`]; below zero is [`ScaleReading::UnderZero`].
/// 4. **An absent scale is an error, never a hang or a zero.** A scale that is unplugged, off or
///    not answering is [`PortError::unavailable`], so a line is never priced at nothing.
pub trait Scale: Send + Sync {
    /// The scale's answer now.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the scale is absent or does not answer.
    fn read(&self) -> impl Future<Output = Result<ScaleReading, PortError>> + Send;
}
