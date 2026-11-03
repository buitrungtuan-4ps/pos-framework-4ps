// `state/reduction-acts.ts`: what the till refuses about a discount or a waive before sending it.

import { describe, expect, it } from "vitest";

import { discountRefusal, waiveRefusal } from "../src/state/reduction-acts";

const MANAGER = { approver_code: "M01", approver_pin: "5678" };

describe("a discount, before it is sent", () => {
  it("is refused when larger than the bill", () => {
    expect(discountRefusal({ amount: 200, due: 100, needsApprover: false, approval: undefined })).not.toBeNull();
  });
  it("is refused when the store asks a manager and none was typed", () => {
    expect(discountRefusal({ amount: 50, due: 100, needsApprover: true, approval: undefined })).not.toBeNull();
  });
  it("goes when it fits the bill and any manager asked for is typed", () => {
    expect(discountRefusal({ amount: 50, due: 100, needsApprover: true, approval: MANAGER })).toBeNull();
    expect(discountRefusal({ amount: 50, due: null, needsApprover: false, approval: undefined })).toBeNull();
  });
});

describe("a waive, before it is sent", () => {
  it("is refused for a fee its rule does not let staff waive", () => {
    expect(waiveRefusal({ waivable: false, asks: false, approval: undefined })).not.toBeNull();
  });
  it("is refused without the manager the store asks for, and goes with one", () => {
    expect(waiveRefusal({ waivable: true, asks: true, approval: undefined })).not.toBeNull();
    expect(waiveRefusal({ waivable: true, asks: true, approval: MANAGER })).toBeNull();
  });
});
