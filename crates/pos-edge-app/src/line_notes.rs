// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A line's guest note, held in memory for the service and nowhere else
//! ([ADR-0157](../../../docs/adr/0157-a-guest-note-lives-in-the-stores-memory-for-the-service.md)).
//!
//! A note is where "for Mr Nguyễn, severe peanut allergy" gets typed: personal data, and often
//! health data. `sales.order_line.added` carries only `note_present` (`pos_proto::text`), so the
//! text needs somewhere to live while the kitchen needs it. This is that place, and it is
//! deliberately small:
//!
//! - **Memory only.** Nothing here writes to the database, the outbox, a backup or a log. A restart
//!   forgets every note, and the log's `note_present` is what lets a screen say one was lost.
//! - **Bounded.** [`NOTE_CAPACITY`] notes at most, the oldest dropped first, so a store that never
//!   closes an order cannot grow this without limit (AGENTS.md: no unbounded cache).
//! - **Forgetful on purpose.** A note goes when its line is voided or its order leaves the open
//!   orders — when no screen can show it any more.
//!
//! [`NoteText`]'s `Debug` never prints the text, so a note cannot reach a log through a `{:?}`.

use std::collections::{HashMap, VecDeque};

use pos_proto::ids::{OrderId, OrderLineId};

/// The longest note the till takes, in characters once trimmed. Long enough for "no onion, nut
/// allergy, bring the sauce on the side"; short enough to read on a ticket over a hot counter.
pub const NOTE_MAX_CHARS: usize = 200;

/// The most notes the edge holds at once. A busy store has a few dozen open lines with notes; two
/// thousand is headroom, and at [`NOTE_MAX_CHARS`] of four-byte characters it is about 1.6 MB.
pub const NOTE_CAPACITY: usize = 2048;

/// Why a note was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteRefused {
    /// Longer than [`NOTE_MAX_CHARS`] once trimmed.
    TooLong,
    /// Carries a control character — a line break, a tab, an escape. A ticket prints a note on one
    /// line, and an escape byte is an instruction to the printer rather than text.
    ControlCharacter,
}

impl NoteRefused {
    /// The refusal in words, for a `400`.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::TooLong => "a note is at most 200 characters",
            Self::ControlCharacter => "a note is one line of text, without control characters",
        }
    }
}

/// A guest note's text, checked: trimmed, non-empty, at most [`NOTE_MAX_CHARS`], one line.
///
/// Personal data. `Debug` reports only its length, so it can sit inside a struct that derives
/// `Debug` without a stray `{:?}` putting it in a log.
#[derive(Clone, PartialEq, Eq)]
pub struct NoteText(Box<str>);

impl NoteText {
    /// Checks a note as a device sent it. `Ok(None)` for an absent or blank note, which is no note.
    ///
    /// # Errors
    ///
    /// [`NoteRefused`] for a note too long, or one carrying a control character.
    pub fn parse(raw: Option<&str>) -> Result<Option<Self>, NoteRefused> {
        let Some(raw) = raw else {
            return Ok(None);
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        if trimmed.chars().any(char::is_control) {
            return Err(NoteRefused::ControlCharacter);
        }
        if trimmed.chars().count() > NOTE_MAX_CHARS {
            return Err(NoteRefused::TooLong);
        }
        Ok(Some(Self(trimmed.into())))
    }

    /// A note that arrived with an inbound order — a marketplace's, a guest's QR order's — made
    /// printable rather than refused. `None` for a blank one.
    ///
    /// The till's note is checked and a bad one refused, because the server is standing there to
    /// fix it. An inbound order has nobody to tell, and refusing a paid order over its note's
    /// formatting would drop the food with the note. So a control character becomes a space and a
    /// note past [`NOTE_MAX_CHARS`] is cut there.
    #[must_use]
    pub fn lenient(raw: &str) -> Option<Self> {
        let printable: String = raw
            .chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .collect();
        let trimmed = printable.trim();
        if trimmed.is_empty() {
            return None;
        }
        let cut: String = trimmed.chars().take(NOTE_MAX_CHARS).collect();
        Some(Self(cut.trim_end().into()))
    }

    /// The note as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Debug for NoteText {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "NoteText({} chars)", self.0.chars().count())
    }
}

/// Every note the edge holds, by line, oldest first.
#[derive(Debug, Default)]
pub struct LineNotes {
    /// Insertion order, so the oldest is dropped first past [`NOTE_CAPACITY`].
    order: VecDeque<OrderLineId>,
    notes: HashMap<OrderLineId, (OrderId, NoteText)>,
}

impl LineNotes {
    /// Holds `note` for `order_line_id`, dropping the oldest note past [`NOTE_CAPACITY`].
    pub fn hold(&mut self, order_id: OrderId, order_line_id: OrderLineId, note: NoteText) {
        if self.notes.insert(order_line_id, (order_id, note)).is_none() {
            self.order.push_back(order_line_id);
        }
        while self.order.len() > NOTE_CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.notes.remove(&oldest);
            }
        }
    }

    /// The note held for a line, if any.
    #[must_use]
    pub fn get(&self, order_line_id: OrderLineId) -> Option<&NoteText> {
        self.notes.get(&order_line_id).map(|(_, note)| note)
    }

    /// Forgets one line's note — a void, or a write that did not commit.
    pub fn forget(&mut self, order_line_id: OrderLineId) {
        if self.notes.remove(&order_line_id).is_some() {
            self.order.retain(|id| *id != order_line_id);
        }
    }

    /// Every line holding a note, with its order, so a caller can ask which are still on a screen.
    #[must_use]
    pub fn held(&self) -> Vec<(OrderLineId, OrderId)> {
        self.order
            .iter()
            .filter_map(|id| self.notes.get(id).map(|(order_id, _)| (*id, *order_id)))
            .collect()
    }

    /// How many notes are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.notes.len()
    }

    /// Whether none are.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use pos_proto::Ulid;
    use pos_proto::ids::{OrderId, OrderLineId};

    use super::{LineNotes, NOTE_CAPACITY, NOTE_MAX_CHARS, NoteRefused, NoteText};

    fn line(n: u128) -> OrderLineId {
        OrderLineId::new(Ulid::from_u128(n))
    }

    fn order(n: u128) -> OrderId {
        OrderId::new(Ulid::from_u128(n))
    }

    fn note(text: &str) -> NoteText {
        NoteText::parse(Some(text))
            .expect("a valid note")
            .expect("not blank")
    }

    #[test]
    fn a_note_is_trimmed_and_a_blank_one_is_no_note() {
        assert_eq!(note("  no onion  ").as_str(), "no onion");
        assert_eq!(NoteText::parse(None), Ok(None));
        assert_eq!(NoteText::parse(Some("   ")), Ok(None));
    }

    #[test]
    fn a_note_is_counted_in_characters_not_bytes() {
        // Two hundred Vietnamese characters are well over two hundred bytes, and they are a note a
        // server can type.
        let longest = "ư".repeat(NOTE_MAX_CHARS);
        assert!(NoteText::parse(Some(&longest)).is_ok());
        let over = "ư".repeat(NOTE_MAX_CHARS + 1);
        assert_eq!(NoteText::parse(Some(&over)), Err(NoteRefused::TooLong));
    }

    #[test]
    fn a_note_with_a_line_break_or_an_escape_is_refused() {
        assert_eq!(
            NoteText::parse(Some("no onion\nno garlic")),
            Err(NoteRefused::ControlCharacter)
        );
        assert_eq!(
            NoteText::parse(Some("\u{1b}@reset")),
            Err(NoteRefused::ControlCharacter)
        );
    }

    #[test]
    fn debug_never_prints_the_text() {
        let rendered = format!("{:?}", note("peanut allergy"));
        assert!(!rendered.contains("peanut"), "{rendered}");
        assert_eq!(rendered, "NoteText(14 chars)");
    }

    #[test]
    fn notes_are_held_by_line_and_forgotten_by_line() {
        let mut notes = LineNotes::default();
        notes.hold(order(1), line(1), note("no onion"));
        notes.hold(order(1), line(2), note("extra spicy"));
        assert_eq!(notes.get(line(1)).map(NoteText::as_str), Some("no onion"));
        notes.forget(line(1));
        assert_eq!(notes.get(line(1)), None);
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn held_lists_each_line_with_its_order_oldest_first() {
        let mut notes = LineNotes::default();
        notes.hold(order(1), line(1), note("a"));
        notes.hold(order(2), line(2), note("b"));
        assert_eq!(notes.held(), vec![(line(1), order(1)), (line(2), order(2))]);
        notes.forget(line(1));
        assert_eq!(notes.held(), vec![(line(2), order(2))]);
    }

    #[test]
    fn an_inbound_note_is_made_printable_rather_than_refused() {
        let flattened = NoteText::lenient("no onion\nno garlic").expect("a note");
        assert_eq!(flattened.as_str(), "no onion no garlic");
        let long = "a".repeat(NOTE_MAX_CHARS + 50);
        let cut = NoteText::lenient(&long).expect("a note");
        assert_eq!(cut.as_str().chars().count(), NOTE_MAX_CHARS);
        assert_eq!(NoteText::lenient(" \t "), None);
    }

    #[test]
    fn past_capacity_the_oldest_note_goes_first() {
        let mut notes = LineNotes::default();
        let capacity = u128::try_from(NOTE_CAPACITY).expect("fits");
        for n in 0..=capacity {
            notes.hold(order(1), line(n), note("x"));
        }
        assert_eq!(notes.len(), NOTE_CAPACITY);
        assert_eq!(notes.get(line(0)), None, "the oldest went");
        assert!(notes.get(line(capacity)).is_some(), "the newest stayed");
    }

    #[test]
    fn holding_a_line_twice_keeps_one_entry() {
        let mut notes = LineNotes::default();
        notes.hold(order(1), line(1), note("a"));
        notes.hold(order(1), line(1), note("b"));
        assert_eq!(notes.len(), 1);
        assert_eq!(notes.get(line(1)).map(NoteText::as_str), Some("b"));
        notes.forget(line(1));
        assert!(notes.is_empty());
    }
}
