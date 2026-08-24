// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Instants and dates — and the two date types the specification insists must never
//! be mixed.
//!
//! # Two concepts, two fields, never mixed
//!
//! `docs/pos-spec.md` §14.1 is emphatic: a store has a day-cutoff hour, so a bill
//! rung at 01:30 belongs to the previous evening's [`BusinessDate`], while a legal
//! invoice uses the [`CalendarDate`]. Rollups, reports and shift closes use one; the
//! country module uses the other.
//!
//! Those are therefore **distinct types with no conversion between them**. Not a
//! newtype pair with `From` impls for convenience — no conversion at all. The
//! fiscal module cannot accidentally accept a business date, because it does not
//! type-check. That turns a rule which would otherwise live in a code review into a
//! compile error.
//!
//! Deriving a business date needs a timezone and a cutoff hour, so it lives in
//! `pos-core`, not here. This module only carries the values.
//!
//! # Why `jiff` and not our own calendar
//!
//! [ADR-0014](../../../docs/adr/0014-datetime-library.md). Note that `jiff` is an
//! implementation detail: it does not appear in any signature below, so a major
//! version bump is not a break in this crate's public API.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An instant, to millisecond precision, always UTC.
///
/// Every timestamp field in the system is one of these, and every one of them is
/// named with a `_time` suffix (`docs/naming-and-api.md` §3.1). `created_at` and
/// `updated_at` are banned outright.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(jiff::Timestamp);

impl Timestamp {
    /// The Unix epoch.
    pub const EPOCH: Self = Self(jiff::Timestamp::UNIX_EPOCH);

    /// Builds an instant from milliseconds since the Unix epoch.
    ///
    /// # Errors
    ///
    /// [`TimeError::Range`] if the value is outside the supported range.
    pub fn from_milliseconds_since_epoch(milliseconds: i64) -> Result<Self, TimeError> {
        jiff::Timestamp::from_millisecond(milliseconds)
            .map(Self)
            .map_err(|_| TimeError::Range)
    }

    /// Milliseconds since the Unix epoch.
    #[must_use]
    pub fn as_milliseconds_since_epoch(self) -> i64 {
        self.0.as_millisecond()
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Timestamp({})", self.0)
    }
}

impl FromStr for Timestamp {
    type Err = TimeError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self).map_err(|_| TimeError::Format)
    }
}

/// Generates a civil-date newtype. Used twice, deliberately, to produce two types
/// that are structurally identical and mutually unconvertible.
macro_rules! civil_date {
    (
        $(#[$meta:meta])*
        $name:ident
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(jiff::civil::Date);

        impl $name {
            /// Builds a date from its parts.
            ///
            /// # Errors
            ///
            /// [`TimeError::Range`] if the combination is not a real date. The 30th
            /// of February is rejected rather than normalised.
            pub fn from_ymd(year: i16, month: u8, day: u8) -> Result<Self, TimeError> {
                let month = i8::try_from(month).map_err(|_| TimeError::Range)?;
                let day = i8::try_from(day).map_err(|_| TimeError::Range)?;
                jiff::civil::Date::new(year, month, day)
                    .map(Self)
                    .map_err(|_| TimeError::Range)
            }

            /// The year.
            #[must_use]
            pub fn year(self) -> i16 {
                self.0.year()
            }

            /// The month, 1 through 12.
            #[must_use]
            pub fn month(self) -> u8 {
                u8::try_from(self.0.month()).unwrap_or(1)
            }

            /// The day of the month, starting at 1.
            #[must_use]
            pub fn day(self) -> u8 {
                u8::try_from(self.0.day()).unwrap_or(1)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0)
            }
        }

        impl FromStr for $name {
            type Err = TimeError;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                text.parse().map(Self).map_err(|_| TimeError::Format)
            }
        }
    };
}

civil_date! {
    /// The trading day a transaction belongs to, using the store's cut-off hour
    /// rather than the calendar.
    ///
    /// Default cut-off is 04:00 local, so a bill rung at 01:30 belongs to the
    /// previous evening. Rollups, reports and shift closes all key on this.
    ///
    /// **There is deliberately no conversion to [`CalendarDate`].** They answer
    /// different questions and the specification requires that they never be
    /// interchanged.
    BusinessDate
}

civil_date! {
    /// The calendar date, with no cut-off applied.
    ///
    /// Legal invoices use this and only this (`docs/adr/0005-country-neutral-core.md`).
    /// **There is deliberately no conversion to [`BusinessDate`].**
    CalendarDate
}

/// Why a time value could not be produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    /// The value is outside the supported range, or is not a real date.
    #[error("value is outside the supported range, or is not a real date")]
    Range,
    /// The text is not in the expected format.
    #[error("expected RFC 3339 for an instant, or YYYY-MM-DD for a date")]
    Format,
}

/// Serialises as a string and parses from one, for any type with `Display` and
/// `FromStr`.
macro_rules! string_serde {
    ($name:ty, $expecting:literal) => {
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = <&str>::deserialize(deserializer)?;
                text.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

string_serde!(Timestamp, "an RFC 3339 instant in UTC");
string_serde!(BusinessDate, "a date as YYYY-MM-DD");
string_serde!(CalendarDate, "a date as YYYY-MM-DD");

#[cfg(test)]
mod tests {
    use super::{BusinessDate, CalendarDate, TimeError, Timestamp};

    #[test]
    fn an_instant_round_trips_through_rfc_3339() {
        let text = "2026-08-14T03:12:45.123Z";
        let parsed: Timestamp = text.parse().expect("parses");
        assert_eq!(parsed.to_string(), text);
    }

    #[test]
    fn milliseconds_survive_a_round_trip() {
        let instant = Timestamp::from_milliseconds_since_epoch(1_787_022_765_123).expect("builds");
        assert_eq!(instant.as_milliseconds_since_epoch(), 1_787_022_765_123);
        let reparsed: Timestamp = instant.to_string().parse().expect("round trip");
        assert_eq!(reparsed, instant);
    }

    #[test]
    fn a_malformed_instant_is_rejected() {
        assert_eq!(
            "14/08/2026".parse::<Timestamp>().expect_err("must reject"),
            TimeError::Format
        );
    }

    #[test]
    fn dates_round_trip_as_iso() {
        let business = BusinessDate::from_ymd(2026, 8, 13).expect("builds");
        assert_eq!(business.to_string(), "2026-08-13");
        assert_eq!(
            "2026-08-13".parse::<BusinessDate>().expect("parses"),
            business
        );
        assert_eq!(
            (business.year(), business.month(), business.day()),
            (2026, 8, 13)
        );
    }

    #[test]
    fn an_impossible_date_is_rejected_rather_than_normalised() {
        // Silently turning 30 February into 2 March would misdate revenue.
        for (year, month, day) in [(2026, 2, 30), (2026, 13, 1), (2026, 4, 31), (2026, 0, 1)] {
            assert_eq!(
                BusinessDate::from_ymd(year, month, day).expect_err("must reject"),
                TimeError::Range,
                "{year}-{month}-{day} should be rejected"
            );
        }
    }

    #[test]
    fn a_leap_day_is_accepted_in_a_leap_year_only() {
        assert!(BusinessDate::from_ymd(2028, 2, 29).is_ok());
        assert!(BusinessDate::from_ymd(2026, 2, 29).is_err());
        // 1900 is not a leap year; 2000 is. A hand-rolled calendar gets this wrong.
        assert!(BusinessDate::from_ymd(1900, 2, 29).is_err());
        assert!(BusinessDate::from_ymd(2000, 2, 29).is_ok());
    }

    #[test]
    fn business_and_calendar_dates_are_separate_types() {
        // They serialise identically and are structurally the same, yet there is no
        // way to pass one where the other is expected. That is the whole point:
        // `docs/pos-spec.md` §14.1's "two concepts, two fields, never mixed" is a
        // compile error rather than a review comment.
        //
        // Uncommenting the line below must fail to compile:
        //   let _: CalendarDate = BusinessDate::from_ymd(2026, 8, 13).expect("d");
        let business = BusinessDate::from_ymd(2026, 8, 13).expect("builds");
        let calendar = CalendarDate::from_ymd(2026, 8, 13).expect("builds");
        assert_eq!(business.to_string(), calendar.to_string());
    }

    #[test]
    fn dates_serialise_as_bare_strings() {
        let business = BusinessDate::from_ymd(2026, 8, 13).expect("builds");
        assert_eq!(
            serde_json::to_string(&business).expect("serialise"),
            r#""2026-08-13""#
        );
    }

    #[test]
    fn instants_serialise_as_bare_strings() {
        let instant: Timestamp = "2026-08-14T03:12:45.123Z".parse().expect("parses");
        assert_eq!(
            serde_json::to_string(&instant).expect("serialise"),
            r#""2026-08-14T03:12:45.123Z""#
        );
    }
}
