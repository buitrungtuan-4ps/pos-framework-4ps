// A receipt template chooses which custom fields print under its header (O8).

import { describe, expect, it } from "vitest";

import { customFieldsOf, printableFields, withCustomFields } from "../src/lib/receipt-templates";

const version = (fields: object[]) => ({ content: { data: { custom_fields: fields } } });

describe("a receipt template's custom fields", () => {
  it("offers order and bill fields once, the latest declaration winning, retired ones left out", () => {
    const fields = printableFields([
      version([
        { custom_field_id: "F1", label_key: "field.table_note", object: "DATA_OBJECT_ORDER" },
        { custom_field_id: "F2", label_key: "field.shift_mood", object: "DATA_OBJECT_SHIFT" },
      ]),
      version([
        { custom_field_id: "F1", label_key: "field.table_note", object: "DATA_OBJECT_ORDER", deprecated: true },
        { custom_field_id: "F3", label_key: "field.phone", object: "DATA_OBJECT_BILL", personal: true },
      ]),
    ]);
    expect(fields).toEqual([{ id: "F3", labelKey: "field.phone", personal: true }]);
  });

  it("adds and removes a field in order, never twice, and drops an empty list", () => {
    const one = withCustomFields({}, ["F1", "F2", "F1"]);
    expect(customFieldsOf(one)).toEqual(["F1", "F2"]);
    expect(withCustomFields(one, [])).toEqual({});
    expect(customFieldsOf(undefined)).toEqual([]);
  });
});
