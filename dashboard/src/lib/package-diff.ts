// What changed between two versions of a package, part by part
// ([ADR-0189](../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 1).
//
// A version never changes once published, so HQ compares two before pinning or rolling back: which
// flows, reactions, screens, time slots, custom fields, lists and object types one adds, removes or
// changes against the other. A part's variants share its id and are compared together, so a new
// variant reads as its part changed. Two values are the same when their JSON is, key order aside.

import type { PackageContent } from "../api/schemas";
import type { MessageKey } from "../i18n";

/** A part of a version the diff lists, with the i18n key of its name. */
export type PackagePart =
  | "flows"
  | "reactions"
  | "screens"
  | "time_slots"
  | "custom_fields"
  | "lists"
  | "object_types";

export const PART_LABEL: Readonly<Record<PackagePart, MessageKey>> = {
  flows: "packageDiff.part.flows",
  reactions: "packageDiff.part.reactions",
  screens: "packageDiff.part.screens",
  time_slots: "packageDiff.part.timeSlots",
  custom_fields: "packageDiff.part.customFields",
  lists: "packageDiff.part.lists",
  object_types: "packageDiff.part.objectTypes",
};

export type Change = "added" | "removed" | "changed";

/** One part that differs: which, its id, and how. */
export type DiffEntry = { readonly part: PackagePart; readonly id: string; readonly change: Change };

/** `value` as JSON with every object's keys sorted, so key order never reads as a change. */
export function canonical(value: unknown): string {
  if (Array.isArray(value)) {
    return `[${value.map(canonical).join(",")}]`;
  }
  if (value !== null && typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>)
      .filter(([, field]) => field !== undefined)
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
    return `{${entries.map(([key, field]) => `${JSON.stringify(key)}:${canonical(field)}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

/** Each part's entries of `content`, grouped by id in the order they first appear. */
function parts(content: PackageContent): Record<PackagePart, Map<string, string>> {
  const grouped = <T>(items: readonly T[] | undefined, id: (item: T) => string): Map<string, string> => {
    const byId = new Map<string, unknown[]>();
    for (const item of items ?? []) {
      const key = id(item);
      byId.set(key, [...(byId.get(key) ?? []), item]);
    }
    return new Map([...byId].map(([key, values]) => [key, canonical(values)]));
  };
  return {
    flows: grouped(content.flows, (variant) => variant.body.flow_id),
    reactions: grouped(content.reactions, (variant) => variant.body.reaction_id),
    screens: grouped(content.screens, (variant) => variant.body.screen_id),
    time_slots: grouped(content.time_slots, (slot) => slot.time_slot_id),
    custom_fields: grouped(content.data?.custom_fields, (field) => field.custom_field_id),
    lists: grouped(content.data?.lists, (list) => list.custom_list_id),
    object_types: grouped(content.data?.object_types, (type) => type.object_type_id),
  };
}

const ORDER: readonly PackagePart[] = [
  "screens",
  "flows",
  "reactions",
  "time_slots",
  "custom_fields",
  "lists",
  "object_types",
];

/** What `later` adds, removes and changes against `earlier`, part by part, in each part's order. */
export function diffVersions(earlier: PackageContent, later: PackageContent): readonly DiffEntry[] {
  const before = parts(earlier);
  const after = parts(later);
  const entries: DiffEntry[] = [];
  for (const part of ORDER) {
    for (const [id, value] of after[part]) {
      const was = before[part].get(id);
      if (was === undefined) {
        entries.push({ part, id, change: "added" });
      } else if (was !== value) {
        entries.push({ part, id, change: "changed" });
      }
    }
    for (const id of before[part].keys()) {
      if (!after[part].has(id)) {
        entries.push({ part, id, change: "removed" });
      }
    }
  }
  return entries;
}
