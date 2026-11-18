// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `scan` node: how a till tells a barcode scanner from typing
//! ([ADR-0222](../../../docs/adr/0222-items-carry-barcodes-and-plus-and-a-counter-can-pay-first.md)
//! decision 2).
//!
//! Phase 6 supports keyboard-wedge scanners: a scanner types the code's digits and Enter, faster
//! than a person can. `keystroke_window_ms` is the most time between two of a scan's keystrokes, so
//! a store sets it for the model of scanner it uses. A node of settings (ADR-0160): absent is the
//! default, and a value that is not a whole number within [`KEYSTROKE_WINDOW_MS`] reads as absent.

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::item_codes::VariableMeasureLayout;
use crate::wire_enum::{Open, is_absent};

/// The values `keystroke_window_ms` may take, in milliseconds.
pub const KEYSTROKE_WINDOW_MS: RangeInclusive<i64> = 20..=200;

/// What `keystroke_window_ms` is where the store sets none.
pub const DEFAULT_KEYSTROKE_WINDOW_MS: i64 = 50;

/// The `scan` node: how a till tells a scanner from typing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct PublishedScan {
    /// The most milliseconds between two keystrokes of one scan. Read it through
    /// [`PublishedScan::keystroke_window_ms`].
    #[serde(
        default,
        deserialize_with = "whole",
        skip_serializing_if = "Option::is_none"
    )]
    pub keystroke_window_ms: Option<i64>,
    /// How a variable-measure code reads (decision 3). Read it through
    /// [`PublishedScan::variable_measure_layout`]; a value that is not a token reads as absent.
    #[serde(default, deserialize_with = "token", skip_serializing_if = "is_absent")]
    pub variable_measure_layout: Open<VariableMeasureLayout>,
}

impl PublishedScan {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "scan";

    /// The most milliseconds between two keystrokes of one scan: the node's number within
    /// [`KEYSTROKE_WINDOW_MS`], and [`DEFAULT_KEYSTROKE_WINDOW_MS`] otherwise.
    #[must_use]
    pub fn keystroke_window_ms(&self) -> i64 {
        self.keystroke_window_ms
            .filter(|window| KEYSTROKE_WINDOW_MS.contains(window))
            .unwrap_or(DEFAULT_KEYSTROKE_WINDOW_MS)
    }

    /// How a variable-measure code reads: an absent or unknown layout reads every code as a plain
    /// one, [`VariableMeasureLayout::None`], the default where a store's country pack seeds none.
    #[must_use]
    pub fn variable_measure_layout(&self) -> VariableMeasureLayout {
        match self.variable_measure_layout.known() {
            VariableMeasureLayout::Unspecified | VariableMeasureLayout::None => {
                VariableMeasureLayout::None
            }
            VariableMeasureLayout::Price5 => VariableMeasureLayout::Price5,
            VariableMeasureLayout::Weight5 => VariableMeasureLayout::Weight5,
        }
    }
}

/// A layout as the node carries it: a token, kept as it came where this release does not know it,
/// and nothing for a value that is not a token.
fn token<'de, D>(deserializer: D) -> Result<Open<VariableMeasureLayout>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)?
        .as_str()
        .map_or_else(Open::default, Open::parse))
}

fn whole<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)?.as_i64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_outside_its_range_or_not_a_number_reads_as_the_default() {
        for (text, window) in [
            ("{}", 50),
            (r#"{ "keystroke_window_ms": 80 }"#, 80),
            (r#"{ "keystroke_window_ms": 19 }"#, 50),
            (r#"{ "keystroke_window_ms": 201 }"#, 50),
            (r#"{ "keystroke_window_ms": "80" }"#, 50),
        ] {
            let node: PublishedScan = serde_json::from_str(text).unwrap_or_default();
            assert_eq!(node.keystroke_window_ms(), window, "{text}");
        }
    }
}
