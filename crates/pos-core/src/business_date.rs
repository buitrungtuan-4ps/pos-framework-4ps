// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Business date, and the timezone arithmetic behind it.
//!
//! [ADR-0014](../../../docs/adr/0014-datetime-library.md) and `docs/pos-spec.md` §14.1. A store has
//! a day-cutoff hour (default 04:00 local), so a bill rung at 01:30 belongs to the *previous*
//! evening's [`BusinessDate`]. Getting this wrong — computing the trading day in the server's
//! timezone rather than the store's — is named in `docs/roadmap.md` P3 as *the* classic
//! revenue-skewing bug, so the derivation is one function with the timezone and cutoff as explicit
//! inputs and nothing implicit.
//!
//! # The algorithm, and the trap it avoids
//!
//! [`derive_business_date`] converts the instant to the store's civil wall-clock time first (the
//! safe direction — an instant always maps to exactly one civil time), then subtracts the cutoff as
//! **civil** arithmetic. It never constructs "today's 04:00 local" as an instant and compares,
//! because that local time does not exist on a spring-forward night and exists twice on a fall-back
//! night — the naive version has a latent bug two nights a year in any DST zone. Staying in civil
//! time means there is nothing to disambiguate.
//!
//! # The ambiguous direction, with one documented policy
//!
//! Daypart windows and shift boundaries run the *other* direction — a local civil time back to an
//! instant — which genuinely is ambiguous across a DST transition. [`resolve_local_time`] applies
//! the policy ADR-0014 fixes once, here, for everywhere: a civil time that a transition **skipped**
//! resolves forward, and one that occurs **twice** resolves to the earlier instant. That is
//! [`Disambiguation::Compatible`](jiff::tz::Disambiguation::Compatible), named explicitly at the
//! call site rather than defaulted.
//!
//! # `business_date` is computed once, at capture
//!
//! ADR-0014: the device stamps `business_date` on the event when it happens and it is never
//! recomputed downstream, because the store's timezone and cutoff are configuration and
//! configuration changes — recomputing later would silently rewrite history. This module is the
//! code that stamp runs through.

use jiff::tz::{Disambiguation, TimeZone};

use pos_proto::time::{BusinessDate, TimeError, Timestamp};

/// Why a clock value could not be produced. `Copy`, like the rest of pos-core's small errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClockError {
    /// The IANA timezone name is not in the bundled database (a typo, or a name that predates the
    /// bundled tzdata). Config validation should have caught it, so reaching here is a bug upstream.
    UnknownTimeZone,
    /// A cutoff hour outside 0–23.
    CutoffHourOutOfRange {
        /// The rejected hour.
        hour: u8,
    },
    /// An instant or derived date fell outside the representable range. Unreachable for a valid
    /// [`Timestamp`], but the types are honest about it rather than panicking.
    OutOfRange,
}

impl core::fmt::Display for ClockError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownTimeZone => f.write_str("unknown IANA timezone name"),
            Self::CutoffHourOutOfRange { hour } => {
                write!(f, "cutoff hour {hour} is not in 0..=23")
            }
            Self::OutOfRange => f.write_str("instant or date outside the representable range"),
        }
    }
}

impl core::error::Error for ClockError {}

impl From<TimeError> for ClockError {
    fn from(_: TimeError) -> Self {
        Self::OutOfRange
    }
}

/// A store's timezone, resolved once from its IANA name against the bundled database.
///
/// Wraps `jiff`'s `TimeZone` so the dependency does not appear in this crate's public signatures
/// (ADR-0014 keeps `jiff` an implementation detail). Construct it when config loads; a bad name
/// fails there, not on every derivation.
#[derive(Debug, Clone)]
pub struct StoreTimeZone(TimeZone);

impl StoreTimeZone {
    /// UTC — the safe default when a store has set no timezone yet.
    #[must_use]
    pub fn utc() -> Self {
        Self(TimeZone::UTC)
    }

    /// Resolves an IANA name (for example `Asia/Ho_Chi_Minh`) against the bundled tzdb.
    ///
    /// # Errors
    ///
    /// [`ClockError::UnknownTimeZone`] if the name is not in the database.
    pub fn from_iana_name(name: &str) -> Result<Self, ClockError> {
        TimeZone::get(name)
            .map(Self)
            .map_err(|_| ClockError::UnknownTimeZone)
    }

    /// The resolved IANA name, when it has one (UTC and fixed offsets may not).
    #[must_use]
    pub fn iana_name(&self) -> Option<&str> {
        self.0.iana_name()
    }
}

/// The store's day-cutoff hour, 0–23. Default 04:00.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutoffHour(u8);

impl CutoffHour {
    /// The specification's default cutoff, 04:00 local.
    pub const DEFAULT: Self = Self(4);

    /// Builds a cutoff hour.
    ///
    /// # Errors
    ///
    /// [`ClockError::CutoffHourOutOfRange`] if `hour` is not in 0–23.
    pub const fn new(hour: u8) -> Result<Self, ClockError> {
        if hour <= 23 {
            Ok(Self(hour))
        } else {
            Err(ClockError::CutoffHourOutOfRange { hour })
        }
    }

    /// The hour, 0–23.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl Default for CutoffHour {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The trading day an instant belongs to, in the store's timezone with its cutoff applied.
///
/// This is the safe direction (instant → civil), so it needs no disambiguation. The cutoff is
/// subtracted as civil arithmetic per ADR-0014, so a fall-back day being 25 hours long is handled
/// without a special case.
///
/// # Errors
///
/// [`ClockError::OutOfRange`] if the instant or the derived date is outside the representable range
/// — unreachable for a valid [`Timestamp`], surfaced rather than panicked on.
pub fn derive_business_date(
    instant: Timestamp,
    zone: &StoreTimeZone,
    cutoff: CutoffHour,
) -> Result<BusinessDate, ClockError> {
    let jiff_instant = jiff::Timestamp::from_millisecond(instant.as_milliseconds_since_epoch())
        .map_err(|_| ClockError::OutOfRange)?;
    let civil = jiff_instant.to_zoned(zone.0.clone()).datetime();
    let shifted = civil
        .checked_sub(jiff::Span::new().hours(i64::from(cutoff.get())))
        .map_err(|_| ClockError::OutOfRange)?;
    let date = shifted.date();
    let month = u8::try_from(date.month()).map_err(|_| ClockError::OutOfRange)?;
    let day = u8::try_from(date.day()).map_err(|_| ClockError::OutOfRange)?;
    BusinessDate::from_ymd(date.year(), month, day).map_err(ClockError::from)
}

/// Resolves a local civil time (as its parts) to the instant it names in the store's timezone.
///
/// For daypart windows and shift boundaries — the ambiguous direction. Applies
/// [`Disambiguation::Compatible`], the one policy ADR-0014 fixes: a skipped time resolves forward, a
/// doubled one to the earlier instant.
///
/// # Errors
///
/// [`ClockError::OutOfRange`] if the parts are not a real civil time or the instant is outside the
/// representable range.
pub fn resolve_local_time(
    year: i16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    zone: &StoreTimeZone,
) -> Result<Timestamp, ClockError> {
    let month = i8::try_from(month).map_err(|_| ClockError::OutOfRange)?;
    let day = i8::try_from(day).map_err(|_| ClockError::OutOfRange)?;
    let hour = i8::try_from(hour).map_err(|_| ClockError::OutOfRange)?;
    let minute = i8::try_from(minute).map_err(|_| ClockError::OutOfRange)?;
    let civil = jiff::civil::DateTime::new(year, month, day, hour, minute, 0, 0)
        .map_err(|_| ClockError::OutOfRange)?;
    let zoned = zone
        .0
        .to_ambiguous_zoned(civil)
        .disambiguate(Disambiguation::Compatible)
        .map_err(|_| ClockError::OutOfRange)?;
    Timestamp::from_milliseconds_since_epoch(zoned.timestamp().as_millisecond())
        .map_err(ClockError::from)
}

#[cfg(test)]
mod tests {
    use super::{ClockError, CutoffHour, StoreTimeZone, derive_business_date, resolve_local_time};
    use pos_proto::time::Timestamp;

    /// Builds an instant from an RFC 3339 string, for readable fixtures.
    fn at(rfc3339: &str) -> Timestamp {
        rfc3339.parse().expect("valid RFC 3339 in a test fixture")
    }

    #[test]
    fn a_bad_timezone_name_is_rejected_at_construction() {
        assert!(matches!(
            StoreTimeZone::from_iana_name("Mars/Olympus_Mons"),
            Err(ClockError::UnknownTimeZone)
        ));
    }

    #[test]
    fn a_real_timezone_resolves() {
        let zone = StoreTimeZone::from_iana_name("Asia/Ho_Chi_Minh").expect("a real zone");
        assert_eq!(zone.iana_name(), Some("Asia/Ho_Chi_Minh"));
    }

    #[test]
    fn a_cutoff_hour_must_be_a_real_hour() {
        assert!(CutoffHour::new(0).is_ok());
        assert!(CutoffHour::new(23).is_ok());
        assert_eq!(
            CutoffHour::new(24),
            Err(ClockError::CutoffHourOutOfRange { hour: 24 })
        );
        assert_eq!(CutoffHour::DEFAULT.get(), 4);
    }

    #[test]
    fn a_bill_before_the_cutoff_belongs_to_the_previous_day() {
        // 01:30 in Ho Chi Minh (UTC+7) with the 04:00 cutoff → the previous evening's date.
        let zone = StoreTimeZone::from_iana_name("Asia/Ho_Chi_Minh").expect("zone");
        // 2026-08-19T01:30 +07:00 is 2026-08-18T18:30Z.
        let instant = at("2026-08-18T18:30:00Z");
        let date = derive_business_date(instant, &zone, CutoffHour::DEFAULT).expect("derives");
        assert_eq!(
            date.to_string(),
            "2026-08-18",
            "01:30 is still the 18th's trading day"
        );
    }

    #[test]
    fn a_bill_after_the_cutoff_belongs_to_the_same_day() {
        let zone = StoreTimeZone::from_iana_name("Asia/Ho_Chi_Minh").expect("zone");
        // 2026-08-19T09:00 +07:00 is 2026-08-19T02:00Z.
        let instant = at("2026-08-19T02:00:00Z");
        let date = derive_business_date(instant, &zone, CutoffHour::DEFAULT).expect("derives");
        assert_eq!(date.to_string(), "2026-08-19");
    }

    #[test]
    fn exactly_at_the_cutoff_is_the_new_day() {
        let zone = StoreTimeZone::from_iana_name("Asia/Ho_Chi_Minh").expect("zone");
        // 2026-08-19T04:00 +07:00 is 2026-08-18T21:00Z. Subtracting 4h civil lands on 00:00 the 19th.
        let instant = at("2026-08-18T21:00:00Z");
        let date = derive_business_date(instant, &zone, CutoffHour::DEFAULT).expect("derives");
        assert_eq!(
            date.to_string(),
            "2026-08-19",
            "the cutoff instant opens the new day"
        );
    }

    #[test]
    fn the_store_timezone_decides_the_day_not_the_servers() {
        // The same instant is a different business date in Honolulu than in Ho Chi Minh — this is
        // the revenue-skewing bug the store timezone exists to prevent.
        let instant = at("2026-08-19T02:00:00Z");
        let hcm = StoreTimeZone::from_iana_name("Asia/Ho_Chi_Minh").expect("zone");
        let hnl = StoreTimeZone::from_iana_name("Pacific/Honolulu").expect("zone");
        let in_hcm = derive_business_date(instant, &hcm, CutoffHour::DEFAULT).expect("derives");
        let in_hnl = derive_business_date(instant, &hnl, CutoffHour::DEFAULT).expect("derives");
        // 09:00 the 19th in HCM (UTC+7); 16:00 the *18th* in Honolulu (UTC-10).
        assert_eq!(in_hcm.to_string(), "2026-08-19");
        assert_eq!(in_hnl.to_string(), "2026-08-18");
    }

    #[test]
    fn a_skipped_local_time_resolves_forward() {
        // US spring-forward 2026: clocks jump 02:00 → 03:00 on 2026-03-08. 02:30 does not exist;
        // Compatible resolves it forward to 03:30 EDT (= 07:30Z).
        let zone = StoreTimeZone::from_iana_name("America/New_York").expect("zone");
        let instant = resolve_local_time(2026, 3, 8, 2, 30, &zone).expect("resolves forward");
        assert_eq!(instant.to_string(), "2026-03-08T07:30:00Z");
    }

    #[test]
    fn a_doubled_local_time_resolves_to_the_earlier_instant() {
        // US fall-back 2026: clocks fall 02:00 → 01:00 on 2026-11-01. 01:30 occurs twice;
        // Compatible resolves to the earlier one, still EDT (UTC-4) = 05:30Z, not EST 06:30Z.
        let zone = StoreTimeZone::from_iana_name("America/New_York").expect("zone");
        let instant = resolve_local_time(2026, 11, 1, 1, 30, &zone).expect("resolves earlier");
        assert_eq!(instant.to_string(), "2026-11-01T05:30:00Z");
    }
}
