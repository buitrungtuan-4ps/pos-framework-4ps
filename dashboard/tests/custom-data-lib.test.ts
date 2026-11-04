// What the custom-data report offers (ADR-0194 decision 7): only non-personal fields, the latest
// version's declarations, and labels from the tenant's grid with the key as the last resort.

import { describe, expect, it } from "vitest";

import type { PackageVersionRecord } from "../src/api/schemas";
import { filterable, formatMilli, gridLabel, listsById, reportTargets, valueText } from "../src/lib/custom-data";

/** One version declaring `data`, with fake labels only. */
function version(data: unknown): PackageVersionRecord {
  return {
    content: {
      package_id: "01J00000000000000000000P01",
      package_version_id: "01J00000000000000000000V01",
      version_label: "March",
      min_edge_version: "0.14.1",
      data,
    },
    create_time: "2026-11-04T00:00:00Z",
  } as unknown as PackageVersionRecord;
}

const OCCASION = {
  custom_field_id: "CF01",
  object: "DATA_OBJECT_ORDER",
  label_key: "data.order.occasion",
  field_type: "CUSTOM_FIELD_TYPE_CHOICE",
  personal: false,
  custom_list_id: "CA01",
};
const PARTY = {
  custom_field_id: "CF02",
  label_key: "data.reservation.party",
  field_type: "CUSTOM_FIELD_TYPE_INT",
  personal: false,
};
const CONTACT = {
  custom_field_id: "CF03",
  label_key: "data.reservation.contact",
  field_type: "CUSTOM_FIELD_TYPE_TEXT",
  personal: true,
};
const DATA = {
  custom_fields: [OCCASION],
  object_types: [{ object_type_id: "OT01", label_key: "data.reservation", fields: [PARTY, CONTACT] }],
  lists: [{ custom_list_id: "CA01", label_key: "data.list", entries: [{ list_entry_id: "EE01", label_key: "data.birthday" }] }],
};

describe("custom data report", () => {
  it("offers each object and type with only its non-personal fields", () => {
    const targets = reportTargets([version(DATA)]);
    expect(targets.map((target) => target.id)).toEqual(["DATA_OBJECT_ORDER", "OT01"]);
    expect(targets[0]?.messageKey).toBe("customData.objectOrder");
    expect(targets[1]?.fields.map((field) => field.custom_field_id)).toEqual(["CF02"]);
  });

  it("never offers a field any version declared personal", () => {
    const later = { ...DATA, custom_fields: [{ ...OCCASION, personal: true }] };
    const targets = reportTargets([version(DATA), version(later)]);
    expect(targets.map((target) => target.id)).toEqual(["OT01"]);
  });

  it("filters by a choice, a yes or no, or a whole number only", () => {
    expect(filterable(OCCASION as never)).toBe(true);
    expect(filterable(PARTY as never)).toBe(true);
    expect(filterable(CONTACT as never)).toBe(false);
  });

  it("labels from the grid in the language, then English, then the key", () => {
    const grid = { "data.birthday": { en: "Birthday", vi: "Sinh nhật" } };
    expect(gridLabel(grid, "data.birthday", "vi")).toBe("Sinh nhật");
    expect(gridLabel(grid, "data.birthday", "ja")).toBe("Birthday");
    expect(gridLabel(grid, "data.wedding", "vi")).toBe("data.wedding");
  });

  it("writes each kind of value in words", () => {
    const words = {
      lists: listsById([version(DATA)]),
      fields: [OCCASION as never],
      label: (key: string) => gridLabel({ "data.birthday": { en: "Birthday" } }, key, "en"),
      yes: "Yes",
      no: "No",
    };
    expect(valueText({ custom_field_id: "CF01", list_entry_id: "EE01" }, words)).toBe("Birthday");
    expect(valueText({ custom_field_id: "CF04", boolean: false }, words)).toBe("No");
    expect(valueText({ custom_field_id: "CF02", integer: 4 }, words)).toBe("4");
    expect(valueText({ custom_field_id: "CF05", quantity: { milli: 1500 } }, words)).toBe("1.5");
    expect(formatMilli(-2000)).toBe("-2");
    expect(formatMilli(250)).toBe("0.25");
  });
});
