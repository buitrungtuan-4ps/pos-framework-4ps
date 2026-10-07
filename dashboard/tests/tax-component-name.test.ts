// A tax component's name is a code of two to eight capital letters and digits, starting with a
// letter ([ADR-0168](../../docs/adr/0168-a-settled-bill-records-its-tax-components.md) decision 2).
//
// The console's copy of the cloud's rule, which the tax grid checks each part against as it is
// typed. What is pinned is the rule's edges, each of which the cloud would refuse on save.

import { describe, expect, it } from "vitest";

import { isTaxComponentName } from "../src/lib/tax-components";

describe("a tax component's name", () => {
  it("is a code of two to eight capital letters and digits", () => {
    for (const name of ["CGST", "SGST", "UTGST", "IGST", "CESS", "GST1", "VA", "ABCDEFG8"]) {
      expect(isTaxComponentName(name), name).toBe(true);
    }
  });

  it("is refused in lower case, as the grid upper-cases it before it is checked", () => {
    for (const name of ["cgst", "Cgst", "cGST"]) {
      expect(isTaxComponentName(name), name).toBe(false);
    }
  });

  it("is refused as words, as one letter, after a digit, and past eight characters", () => {
    for (const name of ["Central GST", "C", "1GST", "ABCDEFGHI", "", " CGST", "C-GST", "ĐGST"]) {
      expect(isTaxComponentName(name), JSON.stringify(name)).toBe(false);
    }
  });
});
