// The till's Details panel types the custom fields a package adds to a bill or an order (O7).

import { describe, expect, it } from "vitest";

import type { CustomField } from "../src/api/schemas";
import { fieldsOn, purposesFor } from "../src/components/ObjectFields";

const field = (id: string, object: string, extra: Partial<CustomField> = {}): CustomField =>
  ({ custom_field_id: id, field_type: "CUSTOM_FIELD_TYPE_TEXT", label_key: `field.${id}`, object, ...extra }) as unknown as CustomField;

const fields = [
  field("note", "DATA_OBJECT_BILL"),
  field("old", "DATA_OBJECT_BILL", { deprecated: true }),
  field("phone", "DATA_OBJECT_BILL", { personal: true, consent_purpose_key: "consent.callback" }),
  field("mood", "DATA_OBJECT_ORDER"),
];

describe("the Details panel", () => {
  it("offers the fields declared on its object, not retired ones", () => {
    expect(fieldsOn(fields, "DATA_OBJECT_BILL").map((entry) => entry.custom_field_id)).toEqual(["note", "phone"]);
    expect(fieldsOn(fields, "DATA_OBJECT_TABLE")).toEqual([]);
  });

  it("asks a consent only for a personal value actually typed", () => {
    expect(purposesFor(fields, { note: "window seat" })).toEqual([]);
    expect(purposesFor(fields, { phone: "  " })).toEqual([]);
    expect(purposesFor(fields, { phone: "0900000000" })).toEqual(["consent.callback"]);
  });
});
