// A custom field's value between the records form and the edge (ADR-0194): typed per field, a
// quantity in thousandths without floating point, empty clearing it, and a malformed value refused.

import { describe, expect, it } from "vitest";

import type { CustomField } from "../src/api/schemas";
import { drafted, milli, wire } from "../src/lib/custom-values";

const field = (field_type: string): CustomField =>
  ({ custom_field_id: "01J0000000000000000000CF01", label_key: "data.x", field_type, personal: false }) as CustomField;

describe("a custom value", () => {
  it("reads a quantity typed in units as thousandths, without floating point", () => {
    expect(milli("1.5")).toBe(1500);
    expect(milli("-0.125")).toBe(-125);
    expect(milli("2")).toBe(2000);
    expect(milli("1.2345")).toBeNull();
    expect(drafted(field("CUSTOM_FIELD_TYPE_QUANTITY"), { custom_field_id: "x", quantity: { milli: -1500 } })).toBe("-1.500");
  });

  it("is sent in the form its field's type takes, and empty clears it", () => {
    expect(wire(field("CUSTOM_FIELD_TYPE_INT"), "4")).toMatchObject({ integer: 4 });
    expect(wire(field("CUSTOM_FIELD_TYPE_INT"), "four")).toBe("invalid");
    expect(wire(field("CUSTOM_FIELD_TYPE_DATE"), "2026-11-06")).toMatchObject({ calendar_date: "2026-11-06" });
    expect(wire(field("CUSTOM_FIELD_TYPE_BOOL"), true)).toMatchObject({ boolean: true });
    expect(wire(field("CUSTOM_FIELD_TYPE_TEXT"), " demo ")).toMatchObject({ text: " demo " });
    expect(wire(field("CUSTOM_FIELD_TYPE_INT"), "  ")).toEqual({ custom_field_id: "01J0000000000000000000CF01" });
  });
});
