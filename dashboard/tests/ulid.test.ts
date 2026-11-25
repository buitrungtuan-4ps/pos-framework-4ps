// The console's ULIDs (`src/lib/ulid.ts`): the specification's layout, so the cloud's `pos-proto`
// types accept every id the package designer mints.

import { describe, expect, it } from "vitest";

import { newUlid, ULID_PATTERN } from "../src/lib/ulid";

describe("newUlid", () => {
  it("writes the time in the first ten characters and the random bits in the last sixteen", () => {
    // The specification's own example time, with every random bit clear and then every one set.
    expect(newUlid(1469918176385, (bytes) => bytes.fill(0))).toBe("01ARYZ6S410000000000000000");
    expect(newUlid(1469918176385, (bytes) => bytes.fill(255))).toBe("01ARYZ6S41ZZZZZZZZZZZZZZZZ");
  });

  it("mints a well-formed id, and a different one each time", () => {
    const first = newUlid();
    const second = newUlid();
    expect(first).toMatch(ULID_PATTERN);
    expect(second).toMatch(ULID_PATTERN);
    expect(first).not.toBe(second);
  });
});
