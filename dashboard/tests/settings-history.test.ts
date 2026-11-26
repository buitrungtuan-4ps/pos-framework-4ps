import { describe, expect, it } from "vitest";

import type { AuditEntry } from "../src/api/schemas";
import { changeOf, restoresFor, type SettingChange } from "../src/lib/settings-history";

const TENANT = "01KTENANTTTTTTTTTTTTTTTTTT";
const STORE_Q1 = "01KSTOREQ1QQQQQQQQQQQQQQQQ";
const STORE_Q2 = "01KSTOREQ2QQQQQQQQQQQQQQQQ";

function entry(id: string, at: number, action: string, slot: string, before: unknown, after: unknown): AuditEntry {
  return {
    id,
    tenant_id: TENANT,
    actor_admin_id: "01KADMIN00000000000000000A",
    actor_email: "ops@example.test",
    actor_role: "ADMIN",
    action,
    entity_type: "setting",
    entity_id: slot,
    before: before === undefined ? null : { value: before },
    after: after === undefined ? null : { value: after },
    request_id: null,
    at_ms: at,
  } as AuditEntry;
}

const changes = [
  entry(
    "A1",
    1,
    "setting.set",
    `einvoice.correction_kind SETTING_SCOPE_STORE ${STORE_Q1}`,
    undefined,
    "EINVOICE_KIND_REPLACEMENT",
  ),
  entry(
    "A2",
    2,
    "setting.set",
    `einvoice.invoiced_correction SETTING_SCOPE_STORE ${STORE_Q1}`,
    "INVOICED_CORRECTION_HQ_ONLY",
    "INVOICED_CORRECTION_REQUEST",
  ),
  entry(
    "A3",
    3,
    "setting.set",
    `billing.card_confirm_approval SETTING_SCOPE_STORE ${STORE_Q1}`,
    "CARD_CONFIRM_APPROVAL_MANAGER",
    "CARD_CONFIRM_APPROVAL_NONE",
  ),
  entry(
    "A4",
    4,
    "setting.set",
    `einvoice.invoiced_correction SETTING_SCOPE_STORE ${STORE_Q2}`,
    undefined,
    "INVOICED_CORRECTION_CANCEL_WINDOW",
  ),
  entry(
    "A5",
    5,
    "setting.set",
    `einvoice.invoiced_correction SETTING_SCOPE_STORE ${STORE_Q1}`,
    "INVOICED_CORRECTION_REQUEST",
    "INVOICED_CORRECTION_MANAGER_UP_TO_LIMIT",
  ),
  entry(
    "A6",
    6,
    "setting.clear",
    `einvoice.correction_kind SETTING_SCOPE_STORE ${STORE_Q1}`,
    "EINVOICE_KIND_REPLACEMENT",
    undefined,
  ),
  entry("X", 7, "setting.publish", `tenant ${TENANT}`, undefined, undefined),
]
  .map(changeOf)
  .filter((change): change is SettingChange => change !== null);

const at = (id: string) => changes.find((change) => change.id === id) as SettingChange;
const q1 = { scope: "SETTING_SCOPE_STORE" as const, scopeId: STORE_Q1 };

describe("the settings history", () => {
  it("reads a write and a clear as changes, and nothing else", () => {
    expect(changes).toHaveLength(6);
    expect(at("A6").after).toBeUndefined();
    expect(at("A2")).toMatchObject({
      key: "einvoice.invoiced_correction",
      scope: "SETTING_SCOPE_STORE",
      before: "INVOICED_CORRECTION_HQ_ONLY",
    });
  });

  it("undoes one setting at its place, back to what it held before the change", () => {
    expect(restoresFor(changes, at("A2"), { kind: "setting", key: "einvoice.invoiced_correction" }, q1)).toEqual([
      {
        key: "einvoice.invoiced_correction",
        scope: "SETTING_SCOPE_STORE",
        scopeId: STORE_Q1,
        value: "INVOICED_CORRECTION_HQ_ONLY",
      },
    ]);
  });

  it("rolls a group back at one store only, leaving other groups and other stores", () => {
    const restores = restoresFor(changes, at("A1"), { kind: "group", node: "einvoice" }, q1);
    expect(restores).toEqual([
      {
        key: "einvoice.invoiced_correction",
        scope: "SETTING_SCOPE_STORE",
        scopeId: STORE_Q1,
        value: "INVOICED_CORRECTION_HQ_ONLY",
      },
    ]);
    // correction_kind was set then cleared: it already holds what it held before, so needs no write.
    expect(restores.some((restore) => restore.key === "billing.card_confirm_approval")).toBe(false);
    expect(restores.some((restore) => restore.scopeId === STORE_Q2)).toBe(false);
  });

  it("rolls every setting back at one store, a cleared value written again", () => {
    const restores = restoresFor(changes, at("A2"), { kind: "all" }, q1);
    expect(restores.map((restore) => [restore.key, restore.value])).toEqual([
      ["billing.card_confirm_approval", "CARD_CONFIRM_APPROVAL_MANAGER"],
      ["einvoice.correction_kind", "EINVOICE_KIND_REPLACEMENT"],
      ["einvoice.invoiced_correction", "INVOICED_CORRECTION_HQ_ONLY"],
    ]);
  });

  it("clears a setting that was not set before the change", () => {
    expect(restoresFor(changes, at("A1"), { kind: "setting", key: "einvoice.correction_kind" }, q1)).toEqual([]);
    const q2 = { scope: "SETTING_SCOPE_STORE" as const, scopeId: STORE_Q2 };
    expect(restoresFor(changes, at("A4"), { kind: "all" }, q2)).toEqual([
      {
        key: "einvoice.invoiced_correction",
        scope: "SETTING_SCOPE_STORE",
        scopeId: STORE_Q2,
        value: undefined,
      },
    ]);
  });
});
