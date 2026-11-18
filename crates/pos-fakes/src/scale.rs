// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The scale fake (ADR-0223): a scale whose load a test sets, as a person at the till would.

use std::sync::{Arc, Mutex};

use pos_ports::scale::{Scale, ScaleReading};
use pos_ports::{PortError, PortName};

use crate::lock;

/// An in-memory scale. Clones share one scale, so a test keeps a handle to put loads on the one an
/// edge reads.
#[derive(Clone, Debug)]
pub struct FakeScale {
    /// What the scale says now; `None` while it is unplugged.
    state: Arc<Mutex<Option<ScaleReading>>>,
}

impl Default for FakeScale {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeScale {
    /// A scale with nothing on it, zeroed.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(Some(ScaleReading::Stable { net_grams: 0 }))),
        }
    }

    /// Settles a load of `net_grams` on the scale.
    pub fn settle(&self, net_grams: u32) {
        self.set(ScaleReading::Stable { net_grams });
    }

    /// Sets what the scale says, as a load moves, overloads it or takes it below zero.
    pub fn set(&self, reading: ScaleReading) {
        *lock(&self.state) = Some(reading);
    }

    /// Unplugs the scale: every read is unavailable until a reading is set again.
    pub fn unplug(&self) {
        *lock(&self.state) = None;
    }
}

impl Scale for FakeScale {
    async fn read(&self) -> Result<ScaleReading, PortError> {
        let reading = *lock(&self.state);
        reading.ok_or_else(|| PortError::unavailable(PortName::Scale, "the scale does not answer"))
    }
}
