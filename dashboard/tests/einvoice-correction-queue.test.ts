import { describe, expect, it } from "vitest";

import { requestTone, shortId, statusFilter } from "../src/screens/einvoices/CorrectionQueue";

describe("HQ's correction queue", () => {
  it("reads approved as done, declined as refused, and waiting as neither", () => {
    expect(requestTone("APPROVED")).toBe("active");
    expect(requestTone("DECLINED")).toBe("danger");
    expect(requestTone("PENDING")).toBe("neutral");
  });

  it("asks the route for one status, or for every request", () => {
    expect(statusFilter("PENDING")).toBe("PENDING");
    expect(statusFilter("APPROVED")).toBe("APPROVED");
    expect(statusFilter("DECLINED")).toBe("DECLINED");
    expect(statusFilter("ALL")).toBeUndefined();
  });

  it("names a bill or a person by the end of its id, and an absent one by a dash", () => {
    expect(shortId("01KBBBBBBBBBBBBBBBBB12345678")).toBe("12345678");
    expect(shortId(null)).toBe("—");
    expect(shortId(undefined)).toBe("—");
  });
});
