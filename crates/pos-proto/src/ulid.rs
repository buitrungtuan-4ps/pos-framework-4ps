// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! ULID — a 128-bit, time-sortable identifier that can be minted offline.
//!
//! Every identifier in the system is one of these. Three properties earn it that
//! place, and all three matter to a POS fleet:
//!
//! * **Offline generation.** A store with no internet still opens orders. An
//!   auto-increment column cannot do that, and would collide the moment a thousand
//!   stores merged their events.
//! * **Time-sortable.** The high 48 bits are a millisecond timestamp, so
//!   lexicographic order is roughly chronological. That is what lets the event feed
//!   page by `page_token=<ulid>` and lets a webhook receiver sort a batch that
//!   arrived out of order.
//! * **Collision-free in practice.** 80 random bits per millisecond per store.
//!
//! The format is implemented here rather than taken from a crate, per
//! [ADR-0007](../../../docs/adr/0007-in-house-vs-dependency.md): write the format,
//! never the cryptographic primitive. Note the division of labour — this module
//! parses, formats and compares. It does **not** generate: minting needs a clock
//! and a random source, which `pos-core` may not touch directly, so generation
//! lives behind the `IdGenerator` port.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Length of the canonical textual form.
///
/// Twenty-six Crockford characters carry 130 bits and a ULID is 128, so the first
/// character encodes only three usable bits — which is why `'8'` and above are
/// rejected there.
pub const ENCODED_LEN: usize = 26;

/// A 128-bit, time-sortable identifier.
///
/// `Ord` is the ordering of the underlying integer, which — because the timestamp
/// occupies the high bits — is also chronological order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Ulid(u128);

impl Ulid {
    /// The all-zero identifier. Useful as a lower bound when paging a feed.
    pub const NIL: Self = Self(0);

    /// Builds an identifier from its two halves.
    ///
    /// `timestamp_ms` is truncated to its low 48 bits and `randomness` to its low
    /// 80, because those are the widths the format defines. Callers holding a
    /// larger clock value are already past the year 10889.
    #[must_use]
    pub const fn from_parts(timestamp_ms: u64, randomness: u128) -> Self {
        let time = ((timestamp_ms as u128) & 0xFFFF_FFFF_FFFF) << 80;
        let random = randomness & 0xFFFF_FFFF_FFFF_FFFF_FFFF;
        Self(time | random)
    }

    /// The raw 128-bit value.
    #[must_use]
    pub const fn to_u128(self) -> u128 {
        self.0
    }

    /// Reconstructs an identifier from a raw 128-bit value.
    #[must_use]
    pub const fn from_u128(value: u128) -> Self {
        Self(value)
    }

    /// Milliseconds since the Unix epoch, from the high 48 bits.
    #[must_use]
    pub const fn timestamp_ms(self) -> u64 {
        // Clippy proves this cannot truncate: 128 bits shifted right by 80 leaves
        // 48, which always fits a u64. No suppression needed.
        (self.0 >> 80) as u64
    }
}

/// Crockford base32 without `I`, `L`, `O` or `U`.
///
/// Those four are excluded so that a human reading an identifier off a screen
/// cannot confuse it with `1` or `0`. This implementation **rejects** them on input
/// rather than folding them onto their look-alikes, which keeps parsing injective:
/// exactly one text maps to each identifier, so a re-serialised identifier is
/// byte-identical to the one received.
const fn encode_digit(value: u8) -> u8 {
    match value {
        0..=9 => b'0' + value,
        10..=17 => b'A' + (value - 10),
        18..=19 => b'J' + (value - 18),
        20..=21 => b'M' + (value - 20),
        22..=26 => b'P' + (value - 22),
        27..=31 => b'V' + (value - 27),
        // Unreachable: every caller masks to five bits. Present because a `u8`
        // match must be exhaustive.
        _ => b'0',
    }
}

/// Inverse of [`encode_digit`]. `None` for any character outside the alphabet,
/// which includes the four deliberately excluded letters.
const fn decode_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'H' => Some(c - b'A' + 10),
        b'J'..=b'K' => Some(c - b'J' + 18),
        b'M'..=b'N' => Some(c - b'M' + 20),
        b'P'..=b'T' => Some(c - b'P' + 22),
        b'V'..=b'Z' => Some(c - b'V' + 27),
        _ => None,
    }
}

impl fmt::Display for Ulid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for position in 0..ENCODED_LEN {
            let shift = 5 * (ENCODED_LEN - 1 - position);
            // Masked to five bits, so the cast is exact.
            let digit = ((self.0 >> shift) & 0x1f) as u8;
            f.write_str(core::str::from_utf8(&[encode_digit(digit)]).unwrap_or("?"))?;
        }
        Ok(())
    }
}

impl fmt::Debug for Ulid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Debug prints the canonical text, not the integer: an identifier in a log
        // line is only useful if it can be pasted into a query.
        write!(f, "Ulid({self})")
    }
}

/// Why a string is not a ULID.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UlidError {
    /// Wrong number of characters.
    #[error("a ULID is {ENCODED_LEN} characters, found {found}")]
    Length {
        /// Length actually supplied.
        found: usize,
    },
    /// A character outside Crockford base32. `I`, `L`, `O` and `U` land here on
    /// purpose.
    #[error("character {found:?} at index {index} is not valid Crockford base32")]
    Character {
        /// Zero-based position of the offending character.
        index: usize,
        /// The character found.
        found: char,
    },
    /// The text is 26 valid characters but encodes a value of 2^128 or more.
    #[error("value exceeds 128 bits: the first character must be '0' through '7'")]
    Overflow,
}

impl FromStr for Ulid {
    type Err = UlidError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let bytes = text.as_bytes();
        if bytes.len() != ENCODED_LEN {
            return Err(UlidError::Length { found: bytes.len() });
        }

        let mut value: u128 = 0;
        for (index, raw) in bytes.iter().enumerate() {
            let digit = decode_digit(raw.to_ascii_uppercase()).ok_or(UlidError::Character {
                index,
                found: char::from(*raw),
            })?;
            // The leading character carries only three of its five bits.
            if index == 0 && digit > 7 {
                return Err(UlidError::Overflow);
            }
            value = (value << 5) | u128::from(digit);
        }
        Ok(Self(value))
    }
}

impl Serialize for Ulid {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Ulid {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl serde::de::Visitor<'_> for Visitor {
            type Value = Ulid;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "a {ENCODED_LEN}-character ULID string")
            }

            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Ulid, E> {
                text.parse().map_err(E::custom)
            }
        }

        deserializer.deserialize_str(Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::{ENCODED_LEN, Ulid, UlidError};

    #[test]
    fn nil_round_trips() {
        let text = "00000000000000000000000000";
        let parsed: Ulid = text.parse().expect("nil parses");
        assert_eq!(parsed, Ulid::NIL);
        assert_eq!(parsed.to_string(), text);
    }

    #[test]
    fn maximum_round_trips() {
        // '7' contributes 0b111 and each of the 25 'Z's contributes 0b11111,
        // which is 128 one-bits exactly.
        let text = "7ZZZZZZZZZZZZZZZZZZZZZZZZZ";
        let parsed: Ulid = text.parse().expect("maximum parses");
        assert_eq!(parsed.to_u128(), u128::MAX);
        assert_eq!(parsed.to_string(), text);
    }

    #[test]
    fn every_alphabet_character_round_trips() {
        for value in 0..32_u8 {
            let ulid = Ulid::from_u128(u128::from(value));
            let text = ulid.to_string();
            assert_eq!(text.len(), ENCODED_LEN);
            assert_eq!(text.parse::<Ulid>().expect("round trip"), ulid);
        }
    }

    #[test]
    fn rejects_the_four_excluded_letters() {
        // Rejecting rather than folding onto look-alikes is what keeps parsing
        // injective, so a re-serialised identifier is byte-identical.
        for letter in ['I', 'L', 'O', 'U'] {
            let text: String = core::iter::once(letter)
                .chain(core::iter::repeat_n('0', ENCODED_LEN - 1))
                .collect();
            let error = text.parse::<Ulid>().expect_err("must reject");
            assert!(
                matches!(error, UlidError::Character { index: 0, .. }),
                "expected a character error for {letter}, got {error:?}"
            );
        }
    }

    #[test]
    fn rejects_values_of_two_to_the_128_or_more() {
        for leading in ['8', '9', 'A', 'Z'] {
            let text: String = core::iter::once(leading)
                .chain(core::iter::repeat_n('0', ENCODED_LEN - 1))
                .collect();
            assert_eq!(
                text.parse::<Ulid>().expect_err("must reject"),
                UlidError::Overflow,
                "leading {leading} should overflow"
            );
        }
    }

    #[test]
    fn rejects_the_wrong_length() {
        assert_eq!(
            "0000".parse::<Ulid>().expect_err("too short"),
            UlidError::Length { found: 4 }
        );
        assert_eq!(
            "000000000000000000000000000"
                .parse::<Ulid>()
                .expect_err("too long"),
            UlidError::Length { found: 27 }
        );
    }

    #[test]
    fn decoding_is_case_insensitive_but_encoding_is_upper() {
        let upper: Ulid = "7ZZZZZZZZZZZZZZZZZZZZZZZZZ".parse().expect("upper");
        let lower: Ulid = "7zzzzzzzzzzzzzzzzzzzzzzzzz".parse().expect("lower");
        assert_eq!(upper, lower);
        assert_eq!(lower.to_string(), "7ZZZZZZZZZZZZZZZZZZZZZZZZZ");
    }

    #[test]
    fn timestamp_survives_a_round_trip_through_the_text_form() {
        let ulid = Ulid::from_parts(1_767_225_600_000, 0x1234_5678_9abc_def0_1234);
        let reparsed: Ulid = ulid.to_string().parse().expect("round trip");
        assert_eq!(reparsed, ulid);
        assert_eq!(reparsed.timestamp_ms(), 1_767_225_600_000);
    }

    #[test]
    fn text_order_matches_chronological_order() {
        // This is the property the event feed's page_token relies on.
        let earlier = Ulid::from_parts(1_000, u128::MAX);
        let later = Ulid::from_parts(1_001, 0);
        assert!(earlier < later);
        assert!(earlier.to_string() < later.to_string());
    }

    #[test]
    fn serialises_as_a_json_string() {
        let ulid = Ulid::from_parts(1_767_225_600_000, 42);
        let json = serde_json::to_string(&ulid).expect("serialise");
        assert_eq!(json, format!("\"{ulid}\""));
        assert_eq!(
            serde_json::from_str::<Ulid>(&json).expect("deserialise"),
            ulid
        );
    }
}
