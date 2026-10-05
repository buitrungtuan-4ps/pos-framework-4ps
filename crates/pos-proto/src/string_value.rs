// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Reading a value the wire carries as a string, from whichever string the deserializer has
//! ([ADR-0166](../../../docs/adr/0166-pos-proto-reads-a-string-value-from-an-owned-string.md)).
//!
//! `<&str>::deserialize` accepts only a string the deserializer can lend for the input's lifetime.
//! `serde_json::from_value` owns its strings, `from_reader` reads through a buffer, and a string
//! with an escape in it is unescaped into one, so each of them refused a valid instant, date,
//! currency or event type with "expected a borrowed string". [`deserialize`] reads all three kinds
//! of string through the type's own parser, and copies none of them.

use core::fmt;

use serde::Deserializer;
use serde::de::{Error, Visitor};

/// Reads a value the wire carries as a string through `parse`, from a borrowed, a transient or an
/// owned string.
///
/// `expecting` names the value, for the refusal a non-string earns: "expected an RFC 3339 instant
/// in UTC" says what was wanted, where "expected a borrowed string" said how it was to be read.
/// What `parse` refuses is refused with its own message, from every kind of string alike.
pub(crate) fn deserialize<'de, D, T, F>(
    deserializer: D,
    expecting: &'static str,
    parse: fn(&str) -> Result<T, F>,
) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    F: fmt::Display,
{
    deserializer.deserialize_str(StringValue { expecting, parse })
}

/// The visitor [`deserialize`] hands the deserializer: what the value is called, and how it is
/// parsed.
struct StringValue<T, F> {
    expecting: &'static str,
    parse: fn(&str) -> Result<T, F>,
}

impl<'de, T, F: fmt::Display> Visitor<'de> for StringValue<T, F> {
    type Value = T;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.expecting)
    }

    /// A string borrowed from the input, read where it lies.
    fn visit_borrowed_str<E: Error>(self, text: &'de str) -> Result<T, E> {
        self.visit_str(text)
    }

    /// A string the deserializer holds only for this call: one it unescaped, or read from a
    /// stream.
    fn visit_str<E: Error>(self, text: &str) -> Result<T, E> {
        (self.parse)(text).map_err(E::custom)
    }

    /// A string the deserializer owns and hands over, as `serde_json::Value` does: read by
    /// reference, then dropped.
    fn visit_string<E: Error>(self, text: String) -> Result<T, E> {
        self.visit_str(&text)
    }
}

#[cfg(test)]
mod tests {
    use core::fmt::Debug;

    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use serde_json::json;

    use crate::envelope::EventTypeRef;
    use crate::events::EventType;
    use crate::money::CurrencyCode;
    use crate::time::{BusinessDate, CalendarDate, Timestamp};

    /// Writes `value`, checks that it is `wire` byte for byte, then reads it back from every kind
    /// of string a deserializer hands over and checks that each read writes `wire` again.
    fn reads_back_from_every_string<T>(value: &T, wire: &str)
    where
        T: Serialize + DeserializeOwned + PartialEq + Debug,
    {
        let written = serde_json::to_vec(value).expect("the value writes");
        assert_eq!(
            core::str::from_utf8(&written),
            Ok(wire),
            "the wire is what it was, byte for byte"
        );
        let tree = serde_json::to_value(value).expect("the value writes as a tree");

        // The first character escaped, as any JSON writer may: the reader unescapes it into a
        // buffer of its own, so the string it hands over lives only for the call.
        let mut characters = wire.chars();
        assert_eq!(characters.next(), Some('"'), "a string on the wire");
        let first = characters.next().expect("a string with something in it");
        let escaped = format!("\"\\u{:04x}{}", u32::from(first), characters.as_str());

        // `from_value` before the readers that buffer, so a borrowed-only read fails there first.
        let reads: [(&str, Result<T, serde_json::Error>); 6] = [
            ("from_str", serde_json::from_str(wire)),
            ("from_slice", serde_json::from_slice(&written)),
            ("from_value", serde_json::from_value(tree)),
            ("from_reader", serde_json::from_reader(written.as_slice())),
            ("from_str, escaped", serde_json::from_str(&escaped)),
            (
                "from_slice, escaped",
                serde_json::from_slice(escaped.as_bytes()),
            ),
        ];
        for (how, read) in reads {
            let read = read.unwrap_or_else(|error| panic!("{how} refused {wire}: {error}"));
            assert_eq!(&read, value, "{how} read another value");
            assert_eq!(
                serde_json::to_vec(&read).expect("the read value writes"),
                written,
                "{how} changed the wire"
            );
        }
    }

    #[test]
    fn an_instant_reads_back_from_every_kind_of_string() {
        let instant: Timestamp = "2026-08-14T03:12:45.123Z".parse().expect("parses");
        reads_back_from_every_string(&instant, "\"2026-08-14T03:12:45.123Z\"");
    }

    #[test]
    fn a_business_date_reads_back_from_every_kind_of_string() {
        let date = BusinessDate::from_ymd(2026, 8, 13).expect("builds");
        reads_back_from_every_string(&date, "\"2026-08-13\"");
    }

    #[test]
    fn a_calendar_date_reads_back_from_every_kind_of_string() {
        let date = CalendarDate::from_ymd(2026, 2, 28).expect("builds");
        reads_back_from_every_string(&date, "\"2026-02-28\"");
    }

    #[test]
    fn a_currency_code_reads_back_from_every_kind_of_string() {
        reads_back_from_every_string(&CurrencyCode::VND, "\"VND\"");
    }

    #[test]
    fn an_event_type_reads_back_from_every_kind_of_string_known_or_not() {
        reads_back_from_every_string(
            &EventTypeRef::from_known(EventType::SalesOrderLineAdded),
            "\"sales.order_line.added\"",
        );
        let newer = EventTypeRef::parse("sales.order.teleported");
        assert_eq!(newer.known(), None, "an event from a newer sender");
        reads_back_from_every_string(&newer, "\"sales.order.teleported\"");
    }

    /// A value the parser refuses is refused from an owned string too: reading more kinds of
    /// string accepts no value the parser did not.
    #[test]
    fn a_malformed_value_is_refused_from_an_owned_string_as_from_text() {
        for (owned, text) in [
            (
                serde_json::from_value::<Timestamp>(json!("14/08/2026")).map(drop),
                serde_json::from_str::<Timestamp>("\"14/08/2026\"").map(drop),
            ),
            (
                serde_json::from_value::<BusinessDate>(json!("2026-02-30")).map(drop),
                serde_json::from_str::<BusinessDate>("\"2026-02-30\"").map(drop),
            ),
            (
                serde_json::from_value::<CalendarDate>(json!("13/08/2026")).map(drop),
                serde_json::from_str::<CalendarDate>("\"13/08/2026\"").map(drop),
            ),
            (
                serde_json::from_value::<CurrencyCode>(json!("vnd")).map(drop),
                serde_json::from_str::<CurrencyCode>("\"vnd\"").map(drop),
            ),
        ] {
            let owned = owned.expect_err("refused from a tree").to_string();
            let text = text.expect_err("refused from text").to_string();
            assert!(
                text.starts_with(&owned),
                "the same refusal, with the position text adds: {owned} / {text}"
            );
        }
    }

    /// A value that is not a string is refused by what was expected, not by how it was to be read.
    #[test]
    fn a_value_that_is_not_a_string_is_refused_by_name() {
        for (refusal, expecting) in [
            (
                serde_json::from_value::<Timestamp>(json!(5)).map(drop),
                "an RFC 3339 instant in UTC",
            ),
            (
                serde_json::from_value::<BusinessDate>(json!(20_260_813)).map(drop),
                "a date as YYYY-MM-DD",
            ),
            (
                serde_json::from_str::<CalendarDate>("true").map(drop),
                "a date as YYYY-MM-DD",
            ),
            (
                serde_json::from_value::<CurrencyCode>(json!(704)).map(drop),
                "an ISO 4217 currency code",
            ),
            (
                serde_json::from_str::<EventTypeRef>("null").map(drop),
                "an event type token",
            ),
        ] {
            let refusal = refusal.expect_err("not a string").to_string();
            assert!(
                refusal.contains(&format!("expected {expecting}")),
                "{refusal}"
            );
        }
    }
}
