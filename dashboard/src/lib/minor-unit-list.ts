// A list of money values typed as comma-separated minor units: the quick-cash notes (ADR-0105) and
// the notes and coins a drawer is counted by (ADR-0186), which Store settings edits side by side.

import type { Json } from "../api/types";

/** The list as the server wants it: positive minor units, ascending, de-duplicated. */
export function parseMinorUnitList(text: string): number[] {
  const parsed = text
    .split(",")
    .map((part) => Number(part.trim()))
    .filter((value) => Number.isFinite(value) && value > 0);
  return [...new Set(parsed)].sort((left, right) => left - right);
}

/** Whether what was typed is not what will be published: a value that is not positive, or a repeat. */
export function minorUnitListRejected(text: string): boolean {
  return text.split(",").filter((part) => part.trim() !== "").length !== parseMinorUnitList(text).length;
}

/** A node's list as the form shows it, or `null` when the node has no such list. */
export function readMinorUnitList(from: Record<string, Json> | null, key: string): string | null {
  const values = from?.[key];
  if (!Array.isArray(values)) {
    return null;
  }
  return values.filter((value): value is number => typeof value === "number" && Number.isFinite(value)).join(", ");
}
