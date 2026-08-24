// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Numeric telemetry.
//!
//! # Machine data only, and that is a legal boundary rather than a preference
//!
//! `docs/roadmap.md` A6 records the commitment: telemetry is machine data, and **no
//! employee-behaviour monitoring feature is to be designed**. This port is where that
//! commitment either holds or quietly stops holding, because a label is exactly where an
//! `employee_id` would arrive without anyone deciding to put it there.
//!
//! The barrier is [`MetricLabelValue`]: lowercase ASCII, digits, `_`, `-` and `.`, and at
//! most [`MetricLabelValue::MAX_LEN`] bytes — **two fewer than a ULID's twenty-six**. So a
//! name, a phone number and an email address are refused by the alphabet, and every
//! identifier in the system is refused by the length, whichever characters its encoding
//! happens to contain. An `EmployeeId` or a `SubjectId` cannot become a label without a
//! deliberate, visible transformation, and there is nowhere for one to arrive by accident.
//!
//! The length is doing the load-bearing work here, not the alphabet. Crockford base32 is
//! uppercase, so rejecting uppercase looks sufficient — but a low-valued ULID encodes as
//! twenty-six ASCII *digits*, which the alphabet permits. A cap below twenty-six is the
//! check that holds for every value rather than for most of them.
//!
//! `docs/pos-spec.md` §12's per-employee void and discount rates are a **dashboard report
//! over the event log**, computed in the cloud from data that has a lawful basis and a
//! retention period. They are not a metric, and the difference is not cosmetic: a metrics
//! backend has no tenant isolation, no retention policy, and no access log.
//!
//! Rejecting ULIDs costs per-store series, and that cost is already paid elsewhere:
//! `docs/capacity-and-reliability.md` turns the monitoring profile off below about fifty
//! stores in favour of sparse sampling straight into PostgreSQL, and per-store figures come
//! from rollups. Label cardinality is multiplicative, so a thousand-store label is not a
//! thousand series — it is a thousand times whatever else is there.
//!
//! # Why this crate does not use `pos_proto::NoPii`
//!
//! That marker is sealed, and its stated meaning is *admissible inside an event payload*.
//! A metric sample is not an event payload: it has different retention, a different
//! destination, and no immutability. Marking metric text with it would widen a guarantee
//! that is load-bearing for the event log, in exchange for a claim the alphabet check above
//! already proves.
//!
//! # No floating point, so a unit is not optional
//!
//! `clippy.toml` bans `f32` and `f64` workspace-wide, so a sample is an `i64` plus a
//! [`MetricUnit`]. That is the better shape anyway — "1500" is ambiguous and
//! "1500 milliseconds" is not — and it is why the monitoring profile in
//! `docs/capacity-and-reliability.md` can be off below fifty stores without losing the
//! meaning of what was already collected.

use core::fmt;

use core::future::Future;

use pos_proto::time::Timestamp;

/// A metric name, in `snake_case` with dotted segments.
///
/// Validated because a name is a boundary (`docs/adr/0010-naming-standard.md`) and because
/// a backend that silently rewrites `Order Count` into `order_count` makes two different
/// call sites produce one indistinguishable series.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MetricName(Box<str>);

/// A label key, in `snake_case`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MetricLabel(Box<str>);

/// A label value.
///
/// The only type this port accepts as a label value, and the mechanical half of
/// `docs/roadmap.md` A6: its alphabet cannot spell a person. See
/// [`MetricLabelValue::parse`].
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MetricLabelValue(Box<str>);

/// Why a metric identifier was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricNameError {
    /// Empty, or an empty dotted segment.
    Empty,
    /// Longer than the type's limit.
    TooLong,
    /// Contained a character outside the permitted set.
    ForbiddenCharacter,
}

impl fmt::Display for MetricNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "a metric identifier must not be empty",
            Self::TooLong => "a metric identifier is too long",
            Self::ForbiddenCharacter => "a metric identifier must be snake_case ASCII",
        })
    }
}

impl core::error::Error for MetricNameError {}

/// Validates `snake_case` ASCII, optionally allowing dots as segment separators.
fn parse_identifier(
    text: &str,
    max_len: usize,
    allow_dots: bool,
) -> Result<Box<str>, MetricNameError> {
    if text.is_empty() {
        return Err(MetricNameError::Empty);
    }
    if text.len() > max_len {
        return Err(MetricNameError::TooLong);
    }
    let permitted = |b: u8| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || (allow_dots && b == b'.')
    };
    if !text.bytes().all(permitted) {
        return Err(MetricNameError::ForbiddenCharacter);
    }
    if text.split('.').any(str::is_empty) {
        return Err(MetricNameError::Empty);
    }
    Ok(text.into())
}

impl MetricName {
    /// The longest name a supported backend accepts.
    pub const MAX_LEN: usize = 128;

    /// Validates and wraps a name.
    ///
    /// # Errors
    ///
    /// [`MetricNameError`] if the name is empty, over [`Self::MAX_LEN`], or contains
    /// anything but lowercase ASCII, digits, `_` and `.`.
    pub fn parse(name: &str) -> Result<Self, MetricNameError> {
        parse_identifier(name, Self::MAX_LEN, true).map(Self)
    }

    /// The name as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl MetricLabel {
    /// The longest label key a supported backend accepts.
    pub const MAX_LEN: usize = 64;

    /// Validates and wraps a label key.
    ///
    /// # Errors
    ///
    /// [`MetricNameError`] if the key is empty, over [`Self::MAX_LEN`], or not lowercase
    /// ASCII `snake_case`. Dots are not permitted in a key.
    pub fn parse(label: &str) -> Result<Self, MetricNameError> {
        parse_identifier(label, Self::MAX_LEN, false).map(Self)
    }

    /// The key as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl MetricLabelValue {
    /// Short on purpose, and **shorter than a ULID**.
    ///
    /// Twenty-four bytes, against a ULID's twenty-six. That is what makes "no identifier
    /// can be a label value" true rather than usually true: relying on the alphabet alone
    /// fails, because `Ulid::from_u128(1)` encodes as twenty-six ASCII digits and every
    /// character of it is permitted. Raising this above 25 reopens that hole, which is why
    /// a test asserts the relationship rather than the number.
    ///
    /// Twenty-four is comfortable for everything a label legitimately holds: the longest
    /// port name is `shipping_dispatch` at seventeen, and a release tag such as
    /// `v1.2.3-beta.1+build9` is twenty.
    pub const MAX_LEN: usize = 24;

    /// Validates and wraps a label value.
    ///
    /// The alphabet is lowercase ASCII, digits, `_`, `-` and `.` — enough for a port name,
    /// a release tag, a status token, or a device class, and not enough for a person. The
    /// length cap is what excludes identifiers: see [`Self::MAX_LEN`]. Those two checks,
    /// not a marker trait, are the guarantee.
    ///
    /// # Errors
    ///
    /// [`MetricNameError`] if the value is empty, over [`Self::MAX_LEN`], or contains a
    /// character outside that set.
    pub fn parse(value: &str) -> Result<Self, MetricNameError> {
        if value.is_empty() {
            return Err(MetricNameError::Empty);
        }
        if value.len() > Self::MAX_LEN {
            return Err(MetricNameError::TooLong);
        }
        if !value.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.')
        }) {
            return Err(MetricNameError::ForbiddenCharacter);
        }
        Ok(Self(value.into()))
    }

    /// The value as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

macro_rules! metric_text_display {
    ($($name:ident),+ $(,)?) => {
        $(
            impl fmt::Display for $name {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str(&self.0)
                }
            }

            impl fmt::Debug for $name {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    write!(f, concat!(stringify!($name), "({})"), self.0)
                }
            }
        )+
    };
}

metric_text_display!(MetricName, MetricLabel, MetricLabelValue);

/// What a sample's number means.
///
/// Mandatory because there is no floating point to hide behind: an `i64` with no unit is a
/// number nobody can act on, and a dashboard threshold set against the wrong unit is an
/// alert that never fires.
///
/// A plain enum rather than a `pos_proto::wire_enum!`, and the absence of an `Unspecified`
/// variant is the reason. A wire enum tolerates a value this build does not recognise,
/// because a sender it cannot control chose it. A unit is chosen by the *caller*, in this
/// process, at the call site — "unspecified" would be a sample nobody can plot, and making
/// it unrepresentable is cheaper than handling it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum MetricUnit {
    /// A monotonically increasing count.
    Count,
    /// A duration in milliseconds.
    Milliseconds,
    /// A size in bytes.
    Bytes,
    /// A queue or backlog depth.
    Items,
    /// Hundredths of a percent, so 50% is 5000 and a tenth of a percent is expressible.
    BasisPoints,
}

impl MetricUnit {
    /// The unit's `snake_case` name, as a metrics backend records it.
    #[must_use]
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Milliseconds => "milliseconds",
            Self::Bytes => "bytes",
            Self::Items => "items",
            Self::BasisPoints => "basis_points",
        }
    }
}

impl fmt::Display for MetricUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_label())
    }
}

/// Asserts the relationship, not the number.
///
/// Raising [`MetricLabelValue::MAX_LEN`] to 26 or beyond makes every identifier in the system
/// spellable as a metric label again. A compile-time assertion rather than a test, because a
/// build that reopens that hole should not produce a binary.
const _: () = assert!(
    MetricLabelValue::MAX_LEN < pos_proto::ulid::ENCODED_LEN,
    "a metric label value must not be able to hold a ULID"
);

/// One measurement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetricSample {
    /// What was measured.
    pub name: MetricName,
    /// The value, in [`Self::unit`].
    pub value: i64,
    /// What the value means.
    pub unit: MetricUnit,
    /// When it was measured. Passed rather than read, so a batch collected during one
    /// scrape carries one timestamp and the series has no jitter the collector invented.
    pub at: Timestamp,
    /// Dimensions. Bounded in count as well as in content, because unbounded label
    /// cardinality is how a metrics backend runs out of memory.
    pub labels: Vec<(MetricLabel, MetricLabelValue)>,
}

impl MetricSample {
    /// The most labels a sample may carry.
    ///
    /// A hard cap rather than a guideline: cardinality is multiplicative, so a sixth label
    /// with ten values does not add ten series, it multiplies the existing count by ten.
    pub const MAX_LABELS: usize = 5;

    /// A sample with no labels.
    #[must_use]
    pub fn new(name: MetricName, value: i64, unit: MetricUnit, at: Timestamp) -> Self {
        Self {
            name,
            value,
            unit,
            at,
            labels: Vec::new(),
        }
    }

    /// Adds a dimension.
    ///
    /// Silently ignores anything past [`Self::MAX_LABELS`] rather than failing, because a
    /// telemetry call must never be the reason a sale fails. Dropping a dimension loses a
    /// facet of a chart; returning an error from here would put a `?` on the money path.
    #[must_use]
    pub fn with_label(mut self, label: MetricLabel, value: MetricLabelValue) -> Self {
        if self.labels.len() < Self::MAX_LABELS {
            self.labels.push((label, value));
        }
        self
    }
}

/// Accepts numeric telemetry.
///
/// # Contract
///
/// 1. **Recording never fails a caller's real work.** An adapter under pressure drops
///    samples and returns `Ok`, or returns [`crate::PortError::resource_exhausted`] for a
///    caller that wants to know; it must not block. `docs/architecture.md` §5 keeps
///    telemetry off the sales path, and a metrics backend being down is not an outage.
/// 2. **Samples are accepted as a batch or not at all.** Partial acceptance would leave a
///    caller unable to say what landed, and unlike the outbox there is nothing here worth
///    the bookkeeping.
/// 3. **Labels are bounded.** An adapter must not expand a label set it was not given.
pub trait MetricsSink: Send + Sync {
    /// Records a batch.
    ///
    /// # Errors
    ///
    /// [`crate::PortError::resource_exhausted`] if the sink is saturated, or
    /// [`crate::PortError::unavailable`] if the backend cannot be reached. Neither is
    /// worth retrying at the call site — telemetry is sampled, and the next scrape is
    /// along shortly.
    fn record(
        &self,
        samples: &[MetricSample],
    ) -> impl Future<Output = Result<(), crate::PortError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{
        MetricLabel, MetricLabelValue, MetricName, MetricNameError, MetricSample, MetricUnit,
    };
    use pos_proto::time::Timestamp;

    fn instant() -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(1_767_225_600_000).expect("builds")
    }

    #[test]
    fn names_are_dotted_snake_case_and_labels_are_not() {
        assert!(MetricName::parse("pos.outbox.depth").is_ok());
        assert!(MetricName::parse("outbox_depth").is_ok());
        assert_eq!(
            MetricName::parse("pos..depth"),
            Err(MetricNameError::Empty),
            "an empty segment is a typo, not a namespace"
        );
        assert_eq!(
            MetricName::parse("OutboxDepth"),
            Err(MetricNameError::ForbiddenCharacter)
        );
        assert_eq!(
            MetricLabel::parse("port.name"),
            Err(MetricNameError::ForbiddenCharacter),
            "dots separate metric namespaces, not label keys"
        );
        assert!(MetricLabel::parse("port_name").is_ok());
    }

    #[test]
    fn a_label_value_cannot_hold_a_person() {
        // This is the mechanical half of roadmap A6. The alphabet is what makes the NoPii
        // implementation on MetricLabelValue true rather than aspirational.
        assert!(MetricLabelValue::parse("event_store").is_ok());
        assert!(MetricLabelValue::parse("v1.2.3-rc1").is_ok());
        assert_eq!(
            MetricLabelValue::parse("Nguyen Van A"),
            Err(MetricNameError::ForbiddenCharacter)
        );
        assert_eq!(
            MetricLabelValue::parse("+84901234567"),
            Err(MetricNameError::ForbiddenCharacter)
        );
        assert_eq!(
            MetricLabelValue::parse("guest@example.com"),
            Err(MetricNameError::ForbiddenCharacter)
        );

        // And no identifier, which stops an EmployeeId or a SubjectId becoming a series by
        // accident, and stops a thousand-store label multiplying every other label's
        // cardinality.
        //
        // Both ends of the ULID space, because the alphabet alone does not do this. A
        // low-valued ULID is twenty-six ASCII digits and every character is permitted; only
        // the length rejects it.
        for value in [
            pos_proto::ulid::Ulid::from_u128(1),
            pos_proto::ulid::Ulid::from_u128(u128::MAX),
        ] {
            let identifier = pos_proto::ids::EmployeeId::new(value);
            assert_eq!(
                MetricLabelValue::parse(&identifier.to_string()),
                Err(MetricNameError::TooLong),
                "{identifier} must not be spellable as a label value"
            );
        }
        let sentence = "a".repeat(MetricLabelValue::MAX_LEN + 1);
        assert_eq!(
            MetricLabelValue::parse(&sentence),
            Err(MetricNameError::TooLong)
        );
    }

    #[test]
    fn label_count_is_capped_and_the_cap_does_not_fail_the_caller() {
        // Dropping a dimension loses a chart facet. Returning an error here would put a
        // `?` on the sales path for the sake of telemetry, which is the wrong trade.
        let mut sample = MetricSample::new(
            MetricName::parse("pos.port.latency").expect("valid"),
            42,
            MetricUnit::Milliseconds,
            instant(),
        );
        for index in 0..(MetricSample::MAX_LABELS + 3) {
            sample = sample.with_label(
                MetricLabel::parse(&format!("dimension_{index}")).expect("valid"),
                MetricLabelValue::parse("value").expect("valid"),
            );
        }
        assert_eq!(sample.labels.len(), MetricSample::MAX_LABELS);
    }

    #[test]
    fn basis_points_exist_because_there_is_no_floating_point() {
        // Half a percent is expressible, which is the whole reason this unit is here
        // rather than a percentage.
        let sample = MetricSample::new(
            MetricName::parse("pos.print.failure_rate").expect("valid"),
            50,
            MetricUnit::BasisPoints,
            instant(),
        );
        assert_eq!(sample.value, 50, "0.5%, exactly, with no float in sight");
    }
}
