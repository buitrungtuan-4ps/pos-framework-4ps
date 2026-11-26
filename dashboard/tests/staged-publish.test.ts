import { describe, expect, it } from "vitest";

import type { SettingDefinition } from "../src/api/schemas";
import { groupSettings, groupsOf, stageableValues, withStaged, writesFor } from "../src/lib/staged-publish";

const setting = (key: string, scopes: string[], extra: Partial<SettingDefinition> = {}): SettingDefinition =>
  ({
    setting_key: key,
    node: key.split(".")[0],
    field: key.split(".")[1],
    kind: "SETTING_KIND_CHOICE",
    scopes,
    since: "0.14.1",
    default: null,
    ...extra,
  }) as unknown as SettingDefinition;

const catalogue = [
  setting("einvoice.correction_kind", ["SETTING_SCOPE_TENANT", "SETTING_SCOPE_STORE"]),
  setting("einvoice.number_source", ["SETTING_SCOPE_TENANT", "SETTING_SCOPE_STORE_GROUP", "SETTING_SCOPE_STORE"], {
    values: ["EINVOICE_NUMBER_SOURCE_POS", "EINVOICE_NUMBER_SOURCE_VENDOR", "EINVOICE_NUMBER_SOURCE_HYBRID"],
    confirm: { value: "EINVOICE_NUMBER_SOURCE_HYBRID", notice: "n" },
  } as Partial<SettingDefinition>),
  setting("billing.part_payments", ["SETTING_SCOPE_TENANT"]),
];

describe("stage a group, then publish it to one place (ADR-0246)", () => {
  it("offers each group once and a group's settings only", () => {
    expect(groupsOf(catalogue)).toEqual(["einvoice", "billing"]);
    expect(groupSettings(catalogue, "einvoice").map((entry) => entry.setting_key)).toEqual([
      "einvoice.correction_kind",
      "einvoice.number_source",
    ]);
  });

  it("never stages a value that needs a recorded confirmation", () => {
    expect(stageableValues(catalogue[1]!)).toEqual(["EINVOICE_NUMBER_SOURCE_POS", "EINVOICE_NUMBER_SOURCE_VENDOR"]);
  });

  it("writes each staged value at the chosen place only, and names one the place cannot take", () => {
    const staged = [
      { key: "einvoice.correction_kind", value: "EINVOICE_KIND_REPLACEMENT" },
      { key: "einvoice.number_source", value: "EINVOICE_NUMBER_SOURCE_VENDOR" },
    ];
    expect(writesFor(catalogue, staged, { scope: "SETTING_SCOPE_STORE_GROUP", scopeId: "G1" })).toEqual({
      writes: [{ key: "einvoice.number_source", value: "EINVOICE_NUMBER_SOURCE_VENDOR" }],
      skipped: ["einvoice.correction_kind"],
    });
    expect(writesFor(catalogue, staged, { scope: "SETTING_SCOPE_STORE", scopeId: "S1" }).writes).toHaveLength(2);
  });

  it("stages a value, and takes it out again when set back to unchanged", () => {
    const one = withStaged(new Map(), "einvoice.number_source", "EINVOICE_NUMBER_SOURCE_VENDOR");
    expect([...one]).toEqual([["einvoice.number_source", "EINVOICE_NUMBER_SOURCE_VENDOR"]]);
    expect(withStaged(one, "einvoice.number_source", undefined).size).toBe(0);
  });
});
