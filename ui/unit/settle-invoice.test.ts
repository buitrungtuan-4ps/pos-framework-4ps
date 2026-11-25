import { describe, expect, it } from "vitest";

import { invoiceLine, taxCodeLooksValid } from "../src/state/settle-act";

describe("the settled panel's e-invoice (ADR-0242)", () => {
  it("names the invoice and its lookup code, and nothing for a bill with none", () => {
    const line = invoiceLine({
      bill_id: "B",
      state: "BILL_STATE_SETTLED",
      print_receipt: false,
      invoice_number: "1C26MAA/00000042",
      invoice_lookup_code: "56789XYZWQ",
    });
    expect(line).toContain("1C26MAA/00000042");
    expect(line).toContain("56789XYZWQ");
    expect(invoiceLine({ bill_id: "B", state: "BILL_STATE_SETTLED", print_receipt: false })).toBeNull();
  });

  it("accepts a Vietnamese tax number with or without its branch, and blank", () => {
    expect(taxCodeLooksValid("0101243150")).toBe(true);
    expect(taxCodeLooksValid("0101243150-001")).toBe(true);
    expect(taxCodeLooksValid("")).toBe(true);
    expect(taxCodeLooksValid("12345")).toBe(false);
    expect(taxCodeLooksValid("0101 243 150")).toBe(false);
  });
});
