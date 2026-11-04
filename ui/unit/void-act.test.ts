// `state/void-act.ts`: the till refuses a void the store asks a manager for, when nobody typed one.

import { describe, expect, it } from "vitest";

import { voidRefusal } from "../src/state/void-act";

describe("a bill's void, before it is sent", () => {
  it("is refused when the store asks a manager and none was typed", () => {
    expect(voidRefusal(true, undefined)).not.toBeNull();
  });

  it("goes when the manager typed their badge and PIN, or none is asked", () => {
    expect(voidRefusal(true, { approver_code: "1001", approver_pin: "1234" })).toBeNull();
    expect(voidRefusal(false, undefined)).toBeNull();
  });
});
