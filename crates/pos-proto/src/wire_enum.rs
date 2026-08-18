// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Wire enums, and the wrapper that makes adding a value to one a non-breaking
//! change.
//!
//! # The rule, and the trap inside it
//!
//! `docs/naming-and-api.md` §3.3 says enum values are `UPPER_SNAKE_CASE`, always
//! carry a `*_UNSPECIFIED` zero value, and that **receivers must treat an unknown
//! value as `*_UNSPECIFIED` instead of failing**. That last clause is what makes
//! adding an enum value additive: an edge running last month's build can still read
//! an event from a cloud that has learned a new payment method.
//!
//! Taken literally, though, it is dangerous. If "unknown becomes unspecified" is the
//! whole story, then `UNSPECIFIED` flows straight into aggregate state and you end up
//! with a settled bill whose payment method is *absent* — which no report can explain
//! and no reconciliation can fix.
//!
//! So [`Open`] does two things the rule does not say out loud:
//!
//! * It **retains the original token**. Re-serialising an event a node did not fully
//!   understand produces byte-identical output, so a store can forward, sign and
//!   store an event from a newer sender without corrupting it.
//! * It offers [`Open::require`], the **domain boundary**. The wire tolerates
//!   `UNSPECIFIED`; the domain refuses it. Tolerance belongs at the edge of the
//!   system, not in the middle of it.

use core::fmt;
use core::marker::PhantomData;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An enumeration that crosses a boundary.
///
/// Implemented by the [`wire_enum!`] macro rather than by hand, so that the
/// `*_UNSPECIFIED` requirement and the token mapping cannot drift apart.
pub trait WireEnum: Copy + Eq + Sized + 'static {
    /// The mandatory zero value.
    const UNSPECIFIED: Self;

    /// Every variant, in declaration order, starting with `UNSPECIFIED`.
    const ALL: &'static [Self];

    /// The `UPPER_SNAKE_CASE` token for this variant.
    fn as_wire(self) -> &'static str;

    /// Parses a token. `None` for anything unrecognised.
    fn from_wire(token: &str) -> Option<Self>;
}

/// A wire enum value that may have come from a newer sender.
///
/// Construct with [`Open::from_known`] when producing a value, and let
/// deserialisation build it when receiving one.
#[derive(Clone, PartialEq, Eq)]
pub struct Open<E> {
    known: E,
    /// The token as received, kept only when it did not map to a known variant.
    raw: Option<Box<str>>,
}

impl<E: WireEnum> Open<E> {
    /// Wraps a value this build understands.
    #[must_use]
    pub const fn from_known(value: E) -> Self {
        Self {
            known: value,
            raw: None,
        }
    }

    /// Parses a token, degrading to `UNSPECIFIED` while keeping the original text.
    #[must_use]
    pub fn parse(token: &str) -> Self {
        match E::from_wire(token) {
            Some(known) => Self { known, raw: None },
            None => Self {
                known: E::UNSPECIFIED,
                raw: Some(Box::from(token)),
            },
        }
    }

    /// The variant this build understands, which is `UNSPECIFIED` for anything it
    /// does not.
    #[must_use]
    pub const fn known(&self) -> E {
        self.known
    }

    /// Whether the value is absent or unrecognised.
    #[must_use]
    pub fn is_unspecified(&self) -> bool {
        self.known == E::UNSPECIFIED
    }

    /// Whether the value came from a sender speaking a newer vocabulary.
    ///
    /// Distinguishing this from a genuine `UNSPECIFIED` is what lets a store raise
    /// "the cloud is sending something I do not understand" rather than silently
    /// treating it as missing.
    #[must_use]
    pub fn is_unrecognised(&self) -> bool {
        self.raw.is_some()
    }

    /// The token this value will serialise as — the original text for anything
    /// unrecognised, so a round trip is byte-identical.
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match &self.raw {
            Some(raw) => raw,
            None => self.known.as_wire(),
        }
    }

    /// The domain boundary: yields the variant, or refuses.
    ///
    /// Every path from the wire into aggregate state goes through here. The wire
    /// tolerates `UNSPECIFIED`; the domain does not.
    ///
    /// # Errors
    ///
    /// [`UnknownEnumValue`] when the value is unspecified or unrecognised.
    pub fn require(&self) -> Result<E, UnknownEnumValue> {
        if self.is_unspecified() {
            return Err(UnknownEnumValue {
                token: self.as_wire().to_owned(),
            });
        }
        Ok(self.known)
    }
}

impl<E: WireEnum> From<E> for Open<E> {
    fn from(value: E) -> Self {
        Self::from_known(value)
    }
}

impl<E: WireEnum> Default for Open<E> {
    fn default() -> Self {
        Self::from_known(E::UNSPECIFIED)
    }
}

impl<E: WireEnum> fmt::Debug for Open<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_unrecognised() {
            write!(f, "Open(unrecognised {:?})", self.as_wire())
        } else {
            write!(f, "Open({})", self.as_wire())
        }
    }
}

impl<E: WireEnum> fmt::Display for Open<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire())
    }
}

impl<E: WireEnum> Serialize for Open<E> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_wire())
    }
}

impl<'de, E: WireEnum> Deserialize<'de> for Open<E> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TokenVisitor<E>(PhantomData<E>);

        impl<E: WireEnum> serde::de::Visitor<'_> for TokenVisitor<E> {
            type Value = Open<E>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an UPPER_SNAKE_CASE enum token")
            }

            fn visit_str<Err: serde::de::Error>(self, token: &str) -> Result<Open<E>, Err> {
                Ok(Open::parse(token))
            }
        }

        deserializer.deserialize_str(TokenVisitor(PhantomData))
    }
}

/// A wire value the domain cannot accept.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("enum value {token:?} is unspecified or not recognised by this build")]
pub struct UnknownEnumValue {
    /// The token as received.
    pub token: String,
}

/// Declares a wire enum together with its token mapping.
///
/// The macro exists so the `*_UNSPECIFIED` zero value and the `UPPER_SNAKE_CASE`
/// tokens cannot be forgotten or drift out of step with the variants. It takes the
/// enum's token prefix once and derives every token from it, so a variant cannot be
/// spelled one way in Rust and another on the wire.
#[macro_export]
macro_rules! wire_enum {
    (
        $(#[$enum_meta:meta])*
        $name:ident, prefix = $prefix:literal;
        $(
            $(#[$variant_meta:meta])*
            $variant:ident = $token:literal
        ),+ $(,)?
    ) => {
        $(#[$enum_meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
        pub enum $name {
            /// Absent, or a value this build does not recognise.
            ///
            /// Tolerated on the wire and refused by the domain — see
            /// [`Open::require`](crate::wire_enum::Open::require).
            #[default]
            Unspecified,
            $(
                $(#[$variant_meta])*
                $variant,
            )+
        }

        impl $crate::wire_enum::WireEnum for $name {
            const UNSPECIFIED: Self = Self::Unspecified;

            const ALL: &'static [Self] = &[Self::Unspecified, $(Self::$variant),+];

            fn as_wire(self) -> &'static str {
                match self {
                    Self::Unspecified => concat!($prefix, "_UNSPECIFIED"),
                    $(Self::$variant => concat!($prefix, "_", $token),)+
                }
            }

            fn from_wire(token: &str) -> Option<Self> {
                match token {
                    t if t == concat!($prefix, "_UNSPECIFIED") => Some(Self::Unspecified),
                    $(t if t == concat!($prefix, "_", $token) => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str($crate::wire_enum::WireEnum::as_wire(*self))
            }
        }
    };
}

#[cfg(test)]
#[expect(
    unreachable_pub,
    reason = "the macro emits `pub` for real call sites; a fixture enum declared \
              inside a test module is unreachable by design"
)]
mod tests {
    use super::{Open, WireEnum};

    wire_enum! {
        /// A fixture enum, standing in for a real one.
        Flavour, prefix = "FLAVOUR";
        Sweet = "SWEET",
        Savoury = "SAVOURY",
    }

    #[test]
    fn tokens_carry_the_prefix() {
        assert_eq!(Flavour::Unspecified.as_wire(), "FLAVOUR_UNSPECIFIED");
        assert_eq!(Flavour::Sweet.as_wire(), "FLAVOUR_SWEET");
    }

    #[test]
    fn unspecified_is_the_first_variant_and_the_default() {
        assert_eq!(Flavour::default(), Flavour::Unspecified);
        assert_eq!(Flavour::ALL.first().copied(), Some(Flavour::Unspecified));
    }

    #[test]
    fn every_variant_round_trips_through_its_token() {
        for variant in Flavour::ALL {
            assert_eq!(Flavour::from_wire(variant.as_wire()), Some(*variant));
        }
    }

    #[test]
    fn an_unknown_token_degrades_instead_of_failing() {
        // The property that makes adding an enum value a non-breaking change.
        let value = Open::<Flavour>::parse("FLAVOUR_UMAMI");
        assert!(value.is_unspecified());
        assert!(value.is_unrecognised());
        assert_eq!(value.known(), Flavour::Unspecified);
    }

    #[test]
    fn an_unknown_token_survives_a_round_trip_byte_for_byte() {
        // So a store can forward, store and sign an event from a newer cloud without
        // corrupting it.
        let json = r#""FLAVOUR_UMAMI""#;
        let value: Open<Flavour> = serde_json::from_str(json).expect("deserialise");
        assert_eq!(serde_json::to_string(&value).expect("serialise"), json);
    }

    #[test]
    fn require_refuses_at_the_domain_boundary() {
        // Tolerance belongs at the edge of the system, not in aggregate state.
        let unrecognised = Open::<Flavour>::parse("FLAVOUR_UMAMI");
        let error = unrecognised.require().expect_err("must refuse");
        assert_eq!(error.token, "FLAVOUR_UMAMI");

        let explicit = Open::<Flavour>::parse("FLAVOUR_UNSPECIFIED");
        assert!(
            explicit.require().is_err(),
            "an explicit UNSPECIFIED must be refused too"
        );

        let known = Open::from_known(Flavour::Sweet);
        assert_eq!(known.require().expect("accepted"), Flavour::Sweet);
    }

    #[test]
    fn an_unrecognised_value_is_distinguishable_from_an_absent_one() {
        // Both are "unspecified", but only one means "the sender knows something we
        // do not", which is worth alerting on rather than ignoring.
        assert!(Open::<Flavour>::parse("FLAVOUR_UMAMI").is_unrecognised());
        assert!(!Open::<Flavour>::parse("FLAVOUR_UNSPECIFIED").is_unrecognised());
        assert!(!Open::from_known(Flavour::Sweet).is_unrecognised());
    }

    #[test]
    fn a_known_value_serialises_as_its_token() {
        let value = Open::from_known(Flavour::Savoury);
        assert_eq!(
            serde_json::to_string(&value).expect("serialise"),
            r#""FLAVOUR_SAVOURY""#
        );
    }
}
