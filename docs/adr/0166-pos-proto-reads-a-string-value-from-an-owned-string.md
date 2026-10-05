# ADR-0166 — pos-proto reads a string value from an owned string

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-10-05
· Relates to [ADR-0014](0014-datetime-library.md),
[ADR-0024](0024-protocol-version-negotiation.md)

The owner approved this change on 2026-10-05, as one of the small fixes in the second phase of the
work plan. Nothing on the wire changes.

## The problem

Five pos-proto types travel as a JSON string: `Timestamp`, `BusinessDate` and `CalendarDate`
(`time.rs`), `CurrencyCode` (`money.rs`) and `EventTypeRef` (`envelope.rs`). Each of the three
files reads the string with `<&str>::deserialize`, which accepts only a string the deserializer
can lend for the input's lifetime. Three ordinary sources cannot lend one:

- `serde_json::from_value`, whose `Value` owns its strings;
- `serde_json::from_reader`, which reads a stream through a buffer;
- a string with an escape in it, such as `"\u0056ND"`, which `from_str` and `from_slice` unescape
  into a buffer.

Each refuses a valid value: "invalid type: string \"VND\", expected a borrowed string". A test
reproduced this for all five types on 2026-10-05. Only an unescaped string read by `from_str` or
`from_slice` works.

No shipped path is known to hit it, and the cost so far is workarounds. The cloud's config
validation and fee module, and the edge's config client and device revocations, write a node out
as text and parse it back instead of calling `from_value`. A #603 test parsed a `Timestamp` by
hand.

## Options considered

| | Option | Cost |
|---|---|---|
| A | Keep the borrowed read, and route every caller through text | Every new caller must know the trap, and a writer that escapes a character is refused today |
| B | Read a `String` (or `Cow<str>`) at each site | An allocation on every read, and the same fix written three times |
| C | **One private visitor in pos-proto that all five read through** | One small module and its tests |

## Decision

Option **C**.

1. A private module, `string_value`, holds one serde visitor. It reads a borrowed string in place, a
   string the deserializer lends only for the call, and an owned one, through the type's own
   parser. It copies none of them.
2. `Timestamp`, `BusinessDate`, `CalendarDate`, `CurrencyCode` and `EventTypeRef` read through it.
   Their parsers, their parse errors and their `Serialize` stay as they are.
3. Its `expecting` names the value. The `string_serde!` macro already carries that text for the
   three time types, and nothing used it. A non-string is refused with, for example, "expected an
   RFC 3339 instant in UTC" instead of "expected a borrowed string".
4. Each type is tested through `from_str`, `from_slice`, `from_value`, `from_reader` and an escaped
   string, and writes back byte for byte.

## Consequences accepted

- Nothing on the wire changes, so `PROTOCOL_VERSION` stays 1 and no snapshot moves. Each type
  accepts what its parser accepts, now from every kind of string, and nothing its parser refuses.
- Besides the reads that now succeed, what was refused stays refused. Only a refusal's wording can
  differ: a non-string is refused by name rather than with "expected a borrowed string".
- The workarounds outside pos-proto keep working, and this change leaves them alone. Their comments
  that say `from_value` cannot read these types are no longer true; each can drop its text round
  trip in a change of its own.
- `Ulid`, `Open<E>` and `CountryCode` already read owned strings, through their own visitors or a
  `Cow<str>`, and are left as they are.
