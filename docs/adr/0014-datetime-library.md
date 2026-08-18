# ADR-0014 — Date, time, and timezone library

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

**Context.** `business_date` is derived from an instant, the store's timezone, and the
store's day-cutoff hour (default 04:00 local). Daypart schedules and happy-hour windows are
expressed in local civil time. Shift boundaries are compared in local civil time. All of
this lives in `pos-core`, which may depend only on `std`, `serde` and pure computation
crates, and may not `unwrap`, `expect` or `panic`.

Two constraints narrow the field more than they first appear. **The store binary must carry
its own timezone database**: Windows ships no tzdb at all, and the edge is a single static
binary installed on machines nobody administers, so depending on the host having
`/usr/share/zoneinfo` is not an option. And **converting civil time to an instant is
genuinely ambiguous** across a daylight-saving transition — a local 02:30 can occur twice or
not at all. Deriving `business_date` runs the safe direction (instant → civil, always
well-defined), but daypart windows and shift boundaries run the ambiguous one.

**Options considered.**

1. `chrono` + `chrono-tz`. The most widely used pair, bundles a tzdb, battle-tested.
   Rejected: ambiguity surfaces as a `LocalResult` that is easy to collapse with a
   convenience method and get silently wrong, and the API surface is large enough that the
   wrong call is always within reach. When the failure mode is "revenue booked to the wrong
   day", an API that makes the mistake easy is the wrong tool.
2. `time` plus a separate tzdb crate. Smaller, but timezone support is bolted on and the
   civil-to-instant story is weaker than either alternative.
3. In-house. Rejected outright. A timezone database is not a *format* — it is a
   continuously edited body of political data. [ADR-0007](0007-in-house-vs-dependency.md)
   draws the line at writing formats, never primitives or data we do not own, and stale
   tzdata misdates revenue in exactly the silent way that rule exists to prevent.
4. **`jiff`, with a bundled timezone database.** Chosen.

**Decision.** Use `jiff` with its bundled-tzdb feature, version-pinned, on the `pos-core`
dependency allow-list. It treats zoned datetimes as a first-class concept and makes
daylight-saving ambiguity an explicit decision at the call site rather than a default that
silently picks one.

Record the disambiguation policy once, in `pos-core`, and apply it everywhere: when a local
civil time does not exist because a DST transition skipped it, resolve **forward**; when it
occurs twice, resolve to the **earlier** instant. A cutoff hour or daypart boundary is a
threshold, not an appointment, so a deterministic rule is worth more than a correct-looking
choice made differently in three places.

**Consequences.**

- One dependency on the allow-list buys correct zoned arithmetic; refreshing tzdata becomes
  a dependency bump with a changelog note rather than a data-maintenance chore.
- The bundled database adds to binary size on both tiers. Accepted: the edge cannot rely on
  the host for this, and correctness here is money.
- The disambiguation policy is a documented decision, so a DST edge case has one answer in
  the codebase instead of three.
- Vietnam and Japan both observe no daylight saving, so none of this is exercised by the
  first two markets. It is paid for on day one anyway, because
  [`architecture.md`](../architecture.md) §7 lists timezone-per-store among the four
  disciplines that cannot be retrofitted cheaply.
