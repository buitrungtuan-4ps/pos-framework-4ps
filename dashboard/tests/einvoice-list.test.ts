import { describe, expect, it } from "vitest";

import { numberPending, numberText, stateTone } from "../src/screens/einvoices/InvoiceList";

describe("the invoice list's submission badge", () => {
  it("reads submitted as done, refused as a fault, and the rest as waiting", () => {
    expect(stateTone("SUBMITTED")).toBe("active");
    expect(stateTone("REJECTED")).toBe("danger");
    expect(stateTone("PENDING")).toBe("neutral");
    expect(stateTone("RETRYING")).toBe("neutral");
  });
});

describe("the invoice list's number", () => {
  it("shows a store's number as it is, and a vendor-numbered invoice's placeholder as pending (ADR-0251)", () => {
    expect(numberText("1C26MAA/00000042")).toBe("1C26MAA/00000042");
    expect(numberPending("1C26MAA/00000042")).toBe(false);
    expect(numberPending("PENDING/01KCCCCCCCCCCCCCCCCCCCCCCC")).toBe(true);
    expect(numberText("PENDING/01KCCCCCCCCCCCCCCCCCCCCCCC")).not.toContain("PENDING/");
  });
});
