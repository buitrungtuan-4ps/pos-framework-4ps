// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The rules every CSV file the cloud writes or reads keeps.

use std::borrow::Cow;

/// What a spreadsheet takes a cell to be a formula from when the cell starts with it: `=`, `+`, `-`,
/// `@`, a tab and a carriage return (OWASP, CSV injection).
const FORMULA_TRIGGERS: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// Whether `text` starts with a formula trigger after any number of `'`, none included.
fn quotes_then_trigger(text: &str) -> bool {
    text.trim_start_matches('\'').starts_with(FORMULA_TRIGGERS)
}

/// A free-text cell as an export writes it, so that a spreadsheet shows it rather than runs it.
///
/// A cell that starts with `=`, `+`, `-`, `@`, a tab or a carriage return is a formula to Excel,
/// Google Sheets and the like: an item named `=HYPERLINK("http://…","click")` would be a live link
/// in the file of whoever exports it and opens it. Such a cell is written with one leading `'`,
/// which a spreadsheet takes as text. A cell that starts with `'`s and then a trigger gets one more
/// `'` too, so that [`unescape_formula`] can give back exactly what was written, and every other cell
/// is written as it is. Only free text comes through here: an id, a token, a date or a number never
/// does, so a negative amount stays a number.
#[must_use]
pub fn escape_formula(text: &str) -> Cow<'_, str> {
    if quotes_then_trigger(text) {
        Cow::Owned(format!("'{text}"))
    } else {
        Cow::Borrowed(text)
    }
}

/// A free-text cell as an import reads it: one leading `'` taken off when the rest of the cell is
/// `'`s and then a formula trigger, which is exactly the `'` [`escape_formula`] added. Any other cell
/// is read as it is.
#[must_use]
pub fn unescape_formula(text: &str) -> &str {
    match text.strip_prefix('\'') {
        Some(rest) if quotes_then_trigger(rest) => rest,
        _ => text,
    }
}

/// Turns a finished `csv::Writer<Vec<u8>>` into its byte buffer, mapping the flush failure a writer's
/// `into_inner` can surface into a `csv::Error` so the caller has one error type.
pub fn finish(writer: csv::Writer<Vec<u8>>) -> Result<Vec<u8>, csv::Error> {
    writer
        .into_inner()
        .map_err(|error| csv::Error::from(error.into_error()))
}
