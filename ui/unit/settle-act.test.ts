// `state/settle-act.ts`: the receipt line never makes up a receipt number (a fiscal fact).

import { describe, expect, it } from "vitest";

import { receiptLine } from "../src/state/settle-act";

describe("the settled panel's receipt line", () => {
  it("names the receipt the edge gave", () => {
    expect(receiptLine({ bill_id: "b", state: "BILL_STATE_SETTLED", print_receipt: true, receipt_number: 42 })).toContain(
      "42",
    );
  });

  it("says the number could not be read, never shows 0, when a re-read found no receipt", () => {
    const line = receiptLine({ bill_id: "b", state: "BILL_STATE_SETTLED", print_receipt: false });
    expect(line).toContain("could not be read");
    expect(line).not.toMatch(/#\s*0\b/);
  });
});
