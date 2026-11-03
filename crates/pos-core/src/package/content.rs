// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Whether a package version's content may be published, and applied
//! ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decisions 3
//! and 6): the cloud checks it before a version is stored, and the edge again before it runs one.
//!
//! The parts (screens, flows, rules, data) add their own checks as they join the content.

use pos_proto::package::{
    LABEL_CHARS_MAX, PackageContent, SCHEDULES_PER_TIME_SLOT_MAX, TIME_SLOTS_MAX, TimeSlot,
};

/// The longest translation key a time slot's name may have, in characters.
pub const LABEL_KEY_CHARS_MAX: usize = 128;

/// The minutes in a day: a schedule's minutes are below it.
const MINUTES_PER_DAY: u16 = 24 * 60;

/// Why a version's content cannot be published or applied. Each names the field at fault in the
/// shape the console's field errors use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentRefusal {
    /// `version_label` is blank or longer than [`LABEL_CHARS_MAX`].
    VersionLabel,
    /// More than [`TIME_SLOTS_MAX`] time slots.
    TooManyTimeSlots,
    /// A `time_slot_id` is empty, too long, or not `a`–`z`, `0`–`9` and `_`.
    TimeSlotId(usize),
    /// Two time slots share an id.
    DuplicateTimeSlot(usize),
    /// A slot's `label_key` is empty, too long, or not `a`–`z`, `0`–`9`, `_` and `.`.
    TimeSlotLabelKey(usize),
    /// A slot has no window, or more than [`SCHEDULES_PER_TIME_SLOT_MAX`].
    TimeSlotSchedules(usize),
    /// A window names no day, a minute outside the day, or starts where it ends.
    Schedule(usize),
}

impl ContentRefusal {
    /// The request field at fault, for the console's field error.
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::VersionLabel => "version_label",
            Self::TooManyTimeSlots => "time_slots",
            Self::TimeSlotId(_) | Self::DuplicateTimeSlot(_) => "time_slots.time_slot_id",
            Self::TimeSlotLabelKey(_) => "time_slots.label_key",
            Self::TimeSlotSchedules(_) | Self::Schedule(_) => "time_slots.schedules",
        }
    }
}

impl core::fmt::Display for ContentRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::VersionLabel => write!(f, "a version label is 1 to {LABEL_CHARS_MAX} characters"),
            Self::TooManyTimeSlots => {
                write!(f, "a package has at most {TIME_SLOTS_MAX} time slots")
            }
            Self::TimeSlotId(index) => write!(
                f,
                "time slot {index}: an id is 1 to {LABEL_CHARS_MAX} characters of a-z, 0-9 and _"
            ),
            Self::DuplicateTimeSlot(index) => {
                write!(f, "time slot {index}: another slot has the same id")
            }
            Self::TimeSlotLabelKey(index) => write!(
                f,
                "time slot {index}: a label key is 1 to {LABEL_KEY_CHARS_MAX} characters of a-z, 0-9, _ and ."
            ),
            Self::TimeSlotSchedules(index) => write!(
                f,
                "time slot {index}: a slot has 1 to {SCHEDULES_PER_TIME_SLOT_MAX} weekly windows"
            ),
            Self::Schedule(index) => write!(
                f,
                "time slot {index}: a window names at least one day, minutes from 0 to 1439, and \
                 does not start where it ends"
            ),
        }
    }
}

impl core::error::Error for ContentRefusal {}

/// Checks a version's content: its label, and each time slot's id, name key and windows.
///
/// # Errors
///
/// The first [`ContentRefusal`] found, in field order.
pub fn check_content(content: &PackageContent) -> Result<(), ContentRefusal> {
    let label = content.version_label.trim();
    if label.is_empty() || label.chars().count() > LABEL_CHARS_MAX {
        return Err(ContentRefusal::VersionLabel);
    }
    if content.time_slots.len() > TIME_SLOTS_MAX {
        return Err(ContentRefusal::TooManyTimeSlots);
    }
    for (index, slot) in content.time_slots.iter().enumerate() {
        check_slot(index, slot)?;
        if content
            .time_slots
            .iter()
            .take(index)
            .any(|earlier| earlier.time_slot_id == slot.time_slot_id)
        {
            return Err(ContentRefusal::DuplicateTimeSlot(index));
        }
    }
    Ok(())
}

fn check_slot(index: usize, slot: &TimeSlot) -> Result<(), ContentRefusal> {
    if !is_token(&slot.time_slot_id, LABEL_CHARS_MAX, |c| c == '_') {
        return Err(ContentRefusal::TimeSlotId(index));
    }
    if !is_token(slot.label_key.as_str(), LABEL_KEY_CHARS_MAX, |c| {
        c == '_' || c == '.'
    }) {
        return Err(ContentRefusal::TimeSlotLabelKey(index));
    }
    if slot.schedules.is_empty() || slot.schedules.len() > SCHEDULES_PER_TIME_SLOT_MAX {
        return Err(ContentRefusal::TimeSlotSchedules(index));
    }
    let window_is_whole = |window: &pos_proto::campaign::PublishedSchedule| {
        window.days & 0b0111_1111 != 0
            && window.start_minute < MINUTES_PER_DAY
            && window.end_minute < MINUTES_PER_DAY
            && window.start_minute != window.end_minute
    };
    if slot.schedules.iter().all(window_is_whole) {
        Ok(())
    } else {
        Err(ContentRefusal::Schedule(index))
    }
}

/// Whether `text` is 1 to `max` characters of `a`–`z`, `0`–`9` and what `also` allows.
fn is_token(text: &str, max: usize, also: impl Fn(char) -> bool) -> bool {
    !text.is_empty()
        && text.chars().count() <= max
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || also(c))
}

#[cfg(test)]
mod tests {
    use pos_proto::campaign::PublishedSchedule;
    use pos_proto::package::{PackageContent, TimeSlot};
    use pos_proto::text::TranslationKey;

    use super::{ContentRefusal, check_content};

    fn content(slots: &str) -> PackageContent {
        serde_json::from_str(&format!(
            r#"{{"package_id":"01J00000000000000000000P01","package_version_id":"01J00000000000000000000V01","version_label":"March","min_edge_version":"0.14.1","time_slots":{slots}}}"#
        ))
        .expect("content parses")
    }

    const LUNCH: &str = r#"{"time_slot_id":"lunch","label_key":"package.slot.lunch","schedules":[{"days":31,"start_minute":660,"end_minute":840}]}"#;

    #[test]
    fn a_label_and_whole_slots_pass() {
        assert_eq!(check_content(&content(&format!("[{LUNCH}]"))), Ok(()));
        assert_eq!(check_content(&content("[]")), Ok(()));
    }

    #[test]
    fn a_blank_or_long_label_is_refused() {
        let mut blank = content("[]");
        blank.version_label = "  ".to_owned();
        assert_eq!(check_content(&blank), Err(ContentRefusal::VersionLabel));
        let mut long = content("[]");
        long.version_label = "x".repeat(65);
        assert_eq!(check_content(&long), Err(ContentRefusal::VersionLabel));
    }

    /// A change made to the one slot of an otherwise whole version.
    type Change = fn(&mut TimeSlot);

    fn first_window(slot: &mut TimeSlot) -> &mut PublishedSchedule {
        slot.schedules.first_mut().expect("the slot has a window")
    }

    /// One slot with one change made to it, and the refusal the change earns.
    #[test]
    fn each_slot_fault_is_named() {
        let mut twice = content(&format!("[{LUNCH},{LUNCH}]"));
        assert_eq!(
            check_content(&twice),
            Err(ContentRefusal::DuplicateTimeSlot(1))
        );
        if let Some(second) = twice.time_slots.get_mut(1) {
            second.time_slot_id = "dinner".to_owned();
        }
        assert_eq!(check_content(&twice), Ok(()));

        let faults: [(Change, ContentRefusal); 7] = [
            (
                |slot| slot.time_slot_id = "Lunch".to_owned(),
                ContentRefusal::TimeSlotId(0),
            ),
            (
                |slot| slot.time_slot_id = String::new(),
                ContentRefusal::TimeSlotId(0),
            ),
            (
                |slot| slot.label_key = TranslationKey::new("Package Slot"),
                ContentRefusal::TimeSlotLabelKey(0),
            ),
            (
                |slot| slot.schedules.clear(),
                ContentRefusal::TimeSlotSchedules(0),
            ),
            (
                |slot| first_window(slot).days = 0,
                ContentRefusal::Schedule(0),
            ),
            (
                |slot| first_window(slot).end_minute = 1440,
                ContentRefusal::Schedule(0),
            ),
            (
                |slot| first_window(slot).end_minute = 660,
                ContentRefusal::Schedule(0),
            ),
        ];
        for (change, refusal) in faults {
            let mut version = content(&format!("[{LUNCH}]"));
            if let Some(slot) = version.time_slots.first_mut() {
                change(slot);
            }
            assert_eq!(check_content(&version), Err(refusal), "{refusal}");
        }
    }

    #[test]
    fn more_slots_than_the_ceiling_are_refused() {
        let slots: Vec<String> = (0..=16)
            .map(|n| LUNCH.replace("\"lunch\"", &format!("\"slot_{n}\"")))
            .collect();
        let many = content(&format!("[{}]", slots.join(",")));
        assert_eq!(check_content(&many), Err(ContentRefusal::TooManyTimeSlots));
    }
}
