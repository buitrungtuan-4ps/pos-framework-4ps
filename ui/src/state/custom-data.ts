// The running package's data part (ADR-0194): its custom fields, object types and lists, as
// `GET /api/package` answers it in `running.data`. The records screen enters values against it, and
// the edge re-checks every value; a store on the default package declares none.

import { createSignal } from "solid-js";

import type { PackageData } from "../api/schemas";

const NONE: PackageData = {};

const [data, setData] = createSignal<PackageData>(NONE);

/** Takes the data part a `GET /api/package` answer carries, or none. */
export function applyCustomData(body: unknown): void {
  const running = (body as { running?: { data?: PackageData } | null } | null)?.running;
  setData(running?.data ?? NONE);
}

/** The running package's data part. */
export const customData = data;

/** The object types a new record may be entered for: those not deprecated. */
export function enterableTypes() {
  return (data().object_types ?? []).filter((kind) => kind.deprecated !== true);
}
