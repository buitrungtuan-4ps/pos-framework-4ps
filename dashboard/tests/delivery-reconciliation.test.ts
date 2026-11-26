import { describe, expect, it } from "vitest";

import { rateText } from "../src/screens/reports/Delivery";

// ADR-0248 decision 3: the commission rate reads as a percentage of what the guest paid.
describe("the delivery reconciliation", () => {
  it("names the rate as a percentage", () => {
    expect(rateText(2_500)).toBe("25%");
    expect(rateText(1_250)).toBe("12.5%");
    expect(rateText(0)).toBe("0%");
  });
});
