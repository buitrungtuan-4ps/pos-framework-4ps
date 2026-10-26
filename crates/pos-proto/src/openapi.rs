// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Schemas for the OpenAPI documents, behind the `openapi` feature
//! ([ADR-0177](../../../docs/adr/0177-openapi-is-the-route-map.md) decision 3).
//!
//! A struct or enum that derives serde's traits derives `utoipa::ToSchema` beside them, because
//! `utoipa` reads the same `#[serde(…)]` attributes. The types whose `Serialize` is written by hand
//! cannot be read that way, so their schemas are written here, each beside a test that compares it
//! with a value the type really serializes: the identifiers and [`Ulid`](crate::ulid::Ulid), the
//! instants and dates, the currency and country codes, an event-type name, and a `wire_enum!` enum
//! and its [`Open`](crate::wire_enum::Open) wrapper. All of them serialize as a string.

use std::borrow::Cow;

use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{KnownFormat, ObjectBuilder, Schema, SchemaFormat, Type};

use crate::envelope::EventTypeRef;
use crate::locale::CountryCode;
use crate::money::CurrencyCode;
use crate::time::{BusinessDate, CalendarDate, Timestamp};
use crate::ulid::Ulid;
use crate::wire_enum::WireEnum;

/// A string schema with a description, and a pattern or a format where the string has one.
pub(crate) fn string(
    description: &str,
    pattern: Option<&str>,
    format: Option<SchemaFormat>,
) -> RefOr<Schema> {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .description(Some(description))
        .pattern(pattern)
        .format(format)
        .into()
}

/// The schema of a `wire_enum!` enum: one of its `UPPER_SNAKE_CASE` tokens, `*_UNSPECIFIED` first.
#[must_use]
pub fn wire_enum<E: WireEnum>() -> RefOr<Schema> {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(E::ALL.iter().map(|variant| variant.as_wire())))
        .into()
}

/// The schema of an [`Open`](crate::wire_enum::Open) token: a string, one of `E`'s tokens or one
/// this build does not know, which is carried through unchanged.
#[must_use]
pub fn open<E: WireEnum>(name: &str) -> RefOr<Schema> {
    let known: Vec<&str> = E::ALL.iter().map(|variant| variant.as_wire()).collect();
    ObjectBuilder::new()
        .schema_type(Type::String)
        .description(Some(format!(
            "A `{name}` token: {}, or one a newer build sent, which is kept as it arrived",
            known.join(", ")
        )))
        .into()
}

/// The name an [`Open`](crate::wire_enum::Open) of `E` is published under, so two `Open`s do not
/// share one.
#[must_use]
pub fn open_name(inner: &str) -> Cow<'static, str> {
    Cow::Owned(format!("Open{inner}"))
}

/// Implements `ToSchema` for a type that serializes as a string, with `$description`, and a
/// pattern or a format where it has one.
macro_rules! string_schema {
    ($name:ty, $description:expr) => {
        $crate::openapi::string_schema!($name, $description, None, None);
    };
    ($name:ty, $description:expr, $pattern:expr, $format:expr) => {
        impl ::utoipa::PartialSchema for $name {
            fn schema() -> ::utoipa::openapi::RefOr<::utoipa::openapi::schema::Schema> {
                $crate::openapi::string($description, $pattern, $format)
            }
        }

        impl ::utoipa::ToSchema for $name {}
    };
}
pub(crate) use string_schema;

/// A ULID's canonical text: 26 Crockford base-32 characters.
pub(crate) const ULID_PATTERN: &str = "^[0-9A-HJKMNP-TV-Z]{26}$";
/// A `YYYY-MM-DD` date.
pub(crate) const DATE_PATTERN: &str = r"^\d{4}-\d{2}-\d{2}$";

// The types whose `Serialize` is written by hand, each a string on the wire. The identifiers get
// theirs where `resource_id!` declares them, and the `wire_enum!` enums where that macro does.
string_schema!(
    Ulid,
    "A ULID: 26 Crockford base-32 characters, ordered by the time it was minted",
    Some(ULID_PATTERN),
    None
);

string_schema!(
    Timestamp,
    "An instant in UTC, as RFC 3339",
    None,
    Some(SchemaFormat::KnownFormat(KnownFormat::DateTime))
);

string_schema!(
    BusinessDate,
    "A business date, YYYY-MM-DD, by the store's own cutoff",
    Some(DATE_PATTERN),
    None
);

string_schema!(
    CalendarDate,
    "A calendar date, YYYY-MM-DD",
    Some(DATE_PATTERN),
    None
);

string_schema!(
    CurrencyCode,
    "An ISO 4217 currency code: three upper-case letters",
    Some("^[A-Z]{3}$"),
    None
);

string_schema!(
    CountryCode,
    "An ISO 3166-1 alpha-2 country code: two upper-case letters",
    Some("^[A-Z]{2}$"),
    None
);

string_schema!(
    EventTypeRef,
    "An event type's dotted name, as the event catalogue writes it"
);

#[cfg(test)]
mod tests {
    use serde::Serialize;
    use utoipa::openapi::RefOr;
    use utoipa::openapi::schema::Schema;
    use utoipa::{PartialSchema, ToSchema};

    use crate::enums::SalesChannel;
    use crate::envelope::EventTypeRef;
    use crate::ids::OrderId;
    use crate::locale::CountryCode;
    use crate::money::CurrencyCode;
    use crate::time::{BusinessDate, CalendarDate, Timestamp};
    use crate::ulid::Ulid;
    use crate::wire_enum::{Open, WireEnum};

    /// The schema as JSON, so a test reads it as a client does.
    fn json(schema: RefOr<Schema>) -> serde_json::Value {
        serde_json::to_value(schema).expect("serialize the schema")
    }

    /// The value as the wire carries it, which must be a string.
    fn wire<T: Serialize>(value: &T) -> String {
        let value = serde_json::to_value(value).expect("serialize the value");
        let Some(text) = value.as_str() else {
            panic!("expected a string on the wire, got {value}");
        };
        text.to_owned()
    }

    /// A schema's keyword, or `null` where it has none.
    fn keyword<'a>(schema: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
        schema.get(name).unwrap_or(&serde_json::Value::Null)
    }

    /// One position of a pattern: the characters it accepts, and how many times it repeats.
    type Atom = (Box<dyn Fn(char) -> bool>, usize);

    /// Reads the small pattern language these schemas use, `^…$` over `[class]{n}`, `\d{n}` and
    /// literal characters, because this crate carries no regular-expression engine.
    fn atoms(pattern: &str) -> Vec<Atom> {
        let body = pattern
            .strip_prefix('^')
            .and_then(|rest| rest.strip_suffix('$'))
            .unwrap_or_else(|| panic!("{pattern} is not anchored"));
        let mut atoms: Vec<Atom> = Vec::new();
        let mut rest = body;
        while let Some(first) = rest.chars().next() {
            let (accepts, after): (Box<dyn Fn(char) -> bool>, &str) = if first == '[' {
                let (class, after) = rest
                    .get(1..)
                    .and_then(|inner| inner.split_once(']'))
                    .unwrap_or_else(|| panic!("{pattern} has an open class"));
                let chars: Vec<char> = class.chars().collect();
                let mut ranges = Vec::new();
                let mut index = 0;
                while let Some(start) = chars.get(index) {
                    if chars.get(index + 1) == Some(&'-') {
                        let end = *chars.get(index + 2).unwrap_or(start);
                        ranges.push((*start, end));
                        index += 3;
                    } else {
                        ranges.push((*start, *start));
                        index += 1;
                    }
                }
                let accepts = move |character: char| {
                    ranges
                        .iter()
                        .any(|(start, end)| (*start..=*end).contains(&character))
                };
                (Box::new(accepts), after)
            } else if let Some(after) = rest.strip_prefix("\\d") {
                (
                    Box::new(|character: char| character.is_ascii_digit()),
                    after,
                )
            } else {
                let after = rest.get(first.len_utf8()..).unwrap_or_default();
                (Box::new(move |character: char| character == first), after)
            };
            let (count, after) = match after.strip_prefix('{') {
                Some(counted) => {
                    let (digits, after) = counted
                        .split_once('}')
                        .unwrap_or_else(|| panic!("{pattern} has an open count"));
                    (digits.parse().expect("a count"), after)
                }
                None => (1, after),
            };
            atoms.push((accepts, count));
            rest = after;
        }
        atoms
    }

    /// Whether `text` is what `pattern` describes.
    fn matches(pattern: &str, text: &str) -> bool {
        let mut characters = text.chars();
        atoms(pattern)
            .iter()
            .all(|(accepts, count)| (0..*count).all(|_| characters.next().is_some_and(accepts)))
            && characters.next().is_none()
    }

    /// The schema says string, and the value really serializes as one, matching its pattern.
    fn agrees<T: Serialize + PartialSchema>(value: &T) {
        let schema = json(T::schema());
        assert_eq!(keyword(&schema, "type"), "string", "{schema}");
        let text = wire(value);
        if let Some(pattern) = keyword(&schema, "pattern").as_str() {
            assert!(matches(pattern, &text), "{text} does not match {pattern}");
        }
    }

    #[test]
    fn each_hand_written_schema_agrees_with_what_the_type_serializes() {
        agrees(&Ulid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef));
        agrees(&OrderId::new(Ulid::from_u128(42)));
        agrees(&Timestamp::from_milliseconds_since_epoch(1_760_000_000_000).expect("an instant"));
        agrees(&BusinessDate::from_ymd(2026, 10, 21).expect("a date"));
        agrees(&CalendarDate::from_ymd(2026, 1, 2).expect("a date"));
        agrees(&CurrencyCode::parse("VND").expect("a currency"));
        agrees(&CountryCode::parse("VN").expect("a country"));
        agrees(&EventTypeRef::parse("sales.order.opened"));
    }

    /// The ULID pattern accepts exactly the 32 digits a ULID is written in, and nothing else.
    #[test]
    fn the_ulid_pattern_is_the_ulid_alphabet() {
        let alphabet: Vec<char> = (0..32_u128)
            .filter_map(|digit| Ulid::from_u128(digit).to_string().chars().last())
            .collect();
        assert_eq!(alphabet.len(), 32);
        for character in ('0'..='9').chain('A'..='Z').chain('a'..='z') {
            assert_eq!(
                matches(super::ULID_PATTERN, &character.to_string().repeat(26)),
                alphabet.contains(&character),
                "{character}"
            );
        }
    }

    #[test]
    fn an_instant_is_published_as_a_date_time() {
        assert_eq!(keyword(&json(Timestamp::schema()), "format"), "date-time");
    }

    #[test]
    fn a_wire_enum_lists_exactly_its_tokens() {
        let schema = json(SalesChannel::schema());
        let listed: Vec<&str> = keyword(&schema, "enum")
            .as_array()
            .expect("an enum")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        let tokens: Vec<&str> = SalesChannel::ALL
            .iter()
            .map(|channel| channel.as_wire())
            .collect();
        assert_eq!(listed, tokens);
        for channel in SalesChannel::ALL {
            assert!(listed.contains(&wire(&Open::from(*channel)).as_str()));
        }
    }

    #[test]
    fn an_open_token_is_a_string_named_for_its_enum() {
        assert_eq!(<Open<SalesChannel> as ToSchema>::name(), "OpenSalesChannel");
        let schema = json(<Open<SalesChannel> as PartialSchema>::schema());
        assert_eq!(keyword(&schema, "type"), "string");
        assert!(
            schema.get("enum").is_none(),
            "an Open token may be one this build does not know"
        );
    }
}
