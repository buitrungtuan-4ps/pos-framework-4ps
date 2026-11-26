// `state/settle-act.ts` `invoiceLine` (ADR-0242, ADR-0251): the settled panel names the e-invoice the
// settle issued, or says its number is pending while the vendor has not given it.

import { describe, expect, it } from "vitest";

import type { BillResponse } from "../src/api/schemas";
import { invoiceLine } from "../src/state/settle-act";

const settled = { bill_id: "01KBILL", state: "BILL_STATE_SETTLED" } as unknown as BillResponse;

describe("invoiceLine", () => {
  it("names the number and the lookup code of an invoice issued with the settle", () => {
    const line = invoiceLine({ ...settled, invoice_number: "1C26MAA/00000042", invoice_lookup_code: "56789XYZWQ" });
    expect(line).toContain("1C26MAA/00000042");
    expect(line).toContain("56789XYZWQ");
  });

  it("says the number is pending, and gives the lookup code, when the vendor numbers it", () => {
    const line = invoiceLine({ ...settled, invoice_number_pending: true, invoice_lookup_code: "56789XYZWQ" });
    expect(line).not.toBeNull();
    expect(line).toContain("56789XYZWQ");
    expect(line).not.toContain("undefined");
  });

  it("says nothing for a deferred invoice, which has no record yet", () => {
    expect(invoiceLine(settled)).toBeNull();
  });
});
