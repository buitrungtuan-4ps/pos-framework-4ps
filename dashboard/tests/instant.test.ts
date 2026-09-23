// The console printed timestamps three ways, and one of them did not pass the locale.
//
// `new Date(ms).toLocaleString()` renders in whatever locale the *browser* is set to, not the one
// the operator chose for the console. This console ships English and Vietnamese, which order the
// day and the month differently, so `09/23` and `23/09` are the same instant written two ways and
// half the readings are wrong with nothing on screen to admit it. Three screens did that: the
// release instant and the per-store effective time on Releases, and the effective time on
// Campaigns — where the whole point of the report is that an operator can *review* what they are
// committing forty shops to ([ADR-0125](../../docs/adr/0125-a-release-is-one-decision-many-writes.md) §26).
//
// The tests below pin both halves of the fix: that the locale is honoured at all, and that the
// month is written as a **name**, which is what makes a misreading impossible rather than merely
// unlikely. A fix that only passed the locale would keep the ambiguous numeric form and pass a test
// that only checked the two locales differ.

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { setLocale } from "../src/i18n";
import { formatInstant } from "../src/lib/format";

// 2026-09-23T14:05:00Z — a day-of-month above 12, so the day and the month cannot be confused for
// each other by coincidence. Rendered in the runner's zone, which is why the assertions below are
// about *which* fields appear and how they are written, never about the wall-clock hour.
const AT_MS = Date.UTC(2026, 8, 23, 14, 5, 0);

beforeEach(() => setLocale("en"));
afterEach(() => setLocale("en"));

describe("formatInstant", () => {
  it("writes the month as a name, so the day and the month cannot be swapped", () => {
    // The defect this replaces was ambiguity, not just a missing argument: `23/09` and `09/23` are
    // both readable as either field. A named month has no second reading in any locale.
    const drawn = formatInstant(AT_MS);
    expect(drawn).toMatch(/Sep/);
    expect(drawn).toContain("2026");
    // And not the all-numeric form the default would have produced.
    expect(drawn).not.toMatch(/\b\d{1,2}\/\d{1,2}\/\d{4}\b/);
  });

  it("follows the console's locale rather than the browser's", () => {
    // The actual bug. `toLocaleString()` with no argument ignores this entirely, so both of these
    // came out identical — in whatever the browser happened to be.
    const english = formatInstant(AT_MS);
    setLocale("vi");
    const vietnamese = formatInstant(AT_MS);
    expect(vietnamese).not.toBe(english);
    expect(vietnamese).toContain("2026");
  });

  it("drops the seconds, which no row in this console shows", () => {
    // `timeStyle: "short"`. The default carries `:00` on every timestamp in the fleet table.
    expect(formatInstant(AT_MS)).not.toMatch(/:\d{2}:\d{2}/);
  });

  it("falls back to the raw number instead of throwing on a value Intl refuses", () => {
    // Inherited from the copy this replaced in `MySessions`, whose note is the reason to keep it: a
    // malformed row should not blank the table it appears in.
    expect(formatInstant(Number.NaN)).toBe("NaN");
    expect(formatInstant(8.64e15 + 1)).toBe(String(8.64e15 + 1));
  });
});
