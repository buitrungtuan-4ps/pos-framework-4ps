import { describe, expect, it } from "vitest";

import { needsSymbol, readInvoicing } from "../src/screens/EInvoices";

describe("the e-invoice symbol screen's reading of a store", () => {
  it("reads the published symbol, the issue moment and the country", () => {
    expect(
      readInvoicing({
        einvoice: { symbol: "1C26MAA", issue_moment: "EINVOICE_ISSUE_MOMENT_IMMEDIATE" },
        locale: { country_code: "VN" },
      }),
    ).toEqual({ symbol: "1C26MAA", issueMoment: "EINVOICE_ISSUE_MOMENT_IMMEDIATE", country: "VN" });
  });

  it("reads a store with nothing published as having no symbol", () => {
    expect(readInvoicing(null)).toEqual({ symbol: null, issueMoment: null, country: null });
    expect(readInvoicing({ einvoice: { symbol: 7 } }).symbol).toBeNull();
  });

  it("says a symbol is needed only when the store issues invoices", () => {
    expect(needsSymbol("EINVOICE_ISSUE_MOMENT_IMMEDIATE")).toBe(true);
    expect(needsSymbol("EINVOICE_ISSUE_MOMENT_BATCH_WINDOW")).toBe(true);
    expect(needsSymbol("EINVOICE_ISSUE_MOMENT_OFF")).toBe(false);
    expect(needsSymbol(null)).toBe(false);
  });
});
