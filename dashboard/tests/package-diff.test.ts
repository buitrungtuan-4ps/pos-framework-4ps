// Comparing two versions of a package before pinning or rolling back (ADR-0189): what one adds,
// removes and changes against the other, part by part, with key order and variant grouping
// handled so only a real difference shows.

import { describe, expect, it } from "vitest";

import type { PackageContent } from "../src/api/schemas";
import { canonical, diffVersions } from "../src/lib/package-diff";

const base: PackageContent = {
  package_id: "01PACKAGEAAAAAAAAAAAAAAAAA",
  package_version_id: "01VERSIONAAAAAAAAAAAAAAAAA",
  version_label: "March",
  min_edge_version: "0.14.1",
  time_slots: [],
  flows: [
    {
      body: {
        flow_id: "seat",
        start_action_id: "floor.table.seat",
        steps: [{ step_id: "guests", screen_id: "guest_count", kind: "STEP_KIND_REQUIRED" }],
      },
    },
  ],
  reactions: [
    {
      body: {
        reaction_id: "open_menu",
        when: "REACTION_TRIGGER_ORDER_OPENED",
        then: { action_id: "ui.screen.open", params: { screen_id: "menu" } },
      },
    },
  ],
} as PackageContent;

describe("comparing two versions", () => {
  it("finds nothing between a version and itself, whatever its key order", () => {
    const reverse = (value: unknown): unknown =>
      Array.isArray(value)
        ? value.map(reverse)
        : value !== null && typeof value === "object"
          ? Object.fromEntries(Object.entries(value).reverse().map(([key, field]) => [key, reverse(field)]))
          : value;
    const reordered = reverse(base) as PackageContent;
    expect(Object.keys(reordered)[0]).toBe("reactions");
    expect(diffVersions(base, reordered)).toEqual([]);
    expect(canonical({ b: 1, a: [{ d: 2, c: 3 }] })).toBe('{"a":[{"c":3,"d":2}],"b":1}');
  });

  it("names what the later version adds, removes and changes, part by part", () => {
    const later = {
      ...base,
      version_label: "April",
      flows: [
        {
          body: {
            flow_id: "seat",
            start_action_id: "floor.table.seat",
            steps: [{ step_id: "guests", screen_id: "guest_count", kind: "STEP_KIND_OPTIONAL" }],
          },
        },
        { body: { flow_id: "pay", start_action_id: "sales.bill.pay", steps: [] } },
      ],
      reactions: [],
      data: {
        custom_fields: [
          {
            custom_field_id: "allergy_note",
            field_type: "CUSTOM_FIELD_TYPE_TEXT",
            label_key: "field.allergy",
            object: "DATA_OBJECT_ORDER",
            personal: false,
          },
        ],
      },
    } as PackageContent;
    expect(diffVersions(base, later)).toEqual([
      { part: "flows", id: "seat", change: "changed" },
      { part: "flows", id: "pay", change: "added" },
      { part: "reactions", id: "open_menu", change: "removed" },
      { part: "custom_fields", id: "allergy_note", change: "added" },
    ]);
    // A rollback reads as the same changes the other way round.
    expect(diffVersions(later, base).map((entry) => entry.change)).toEqual([
      "changed",
      "removed",
      "added",
      "removed",
    ]);
  });

  it("reads a new variant of a part as its part changed", () => {
    const flow = base.flows?.[0];
    const later = {
      ...base,
      flows: [...(base.flows ?? []), { ...flow, selector: { sales_channel: "SALES_CHANNEL_TAKEAWAY" } }],
    } as PackageContent;
    expect(diffVersions(base, later)).toEqual([{ part: "flows", id: "seat", change: "changed" }]);
  });
});
