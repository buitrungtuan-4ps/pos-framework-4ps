// The floor's areas as the store published them (ADR-0211): an area may sit inside another, and a
// floor is simply a top-level area. The cloud publishes the areas already in tree order, each with its
// `depth` and its `path` (the ids of the areas it sits inside, from the top), so this module never
// walks the tree. It reads the order, and names a breadcrumb by looking each id of a path up.
//
// The floor screen shows the top-level areas as tabs once there is more than one, and the levels
// below as breadcrumb-headed sections with their tables. A store whose areas are all at the top is
// not nested, and draws exactly what it drew before: every area, one section each, no tabs.

import { createMemo, createRoot, createSignal } from "solid-js";

import type { FloorResponse } from "../api/schemas";
import type { FloorArea, TableCard } from "./store";

/** One published area: where it sits, and the ids of its tables in the order they were published. */
export interface AreaPlace {
  readonly id: string;
  readonly name: string;
  readonly depth: number;
  readonly path: readonly string[];
  readonly tableIds: readonly string[];
}

const [places, setPlaces] = createSignal<readonly AreaPlace[]>([]);
const [chosenTop, setChosenTop] = createSignal<string | null>(null);

/**
 * Keeps the published areas, in published order. Called with the plan `applyFloor` applies, and only
 * when it applies one, so the floor is never left without the areas its tables came from.
 */
export function applyAreaPlaces(areas: FloorResponse["floor"]["areas"]): void {
  setPlaces(
    (areas ?? []).map((area) => ({
      id: area.area_id,
      name: area.name,
      depth: area.depth ?? 0,
      path: area.path ?? [],
      tableIds: (area.tables ?? []).map((table) => table.table_id),
    })),
  );
}

/** The top-level area an area sits under: the first id of its path, or itself at the top. */
function topOf(place: AreaPlace): string {
  return place.path[0] ?? place.id;
}

const derived = createRoot(() => {
  const nested = createMemo(() => places().some((place) => place.depth > 0));
  const tops = createMemo(() => places().filter((place) => place.depth === 0));
  const names = createMemo(() => new Map(places().map((place) => [place.id, place.name])));
  return { nested, tops, names };
});

/** Whether any area sits inside another. A store that is not nested draws as it always has. */
export function floorIsNested(): boolean {
  return derived.nested();
}

/** The top-level areas, the tabs, in published order. */
export function topAreas(): readonly AreaPlace[] {
  return derived.tops();
}

/** The tab shown: the one chosen on this device while it is still published, else the first. */
export function activeTop(): string | null {
  const tops = derived.tops();
  const chosen = chosenTop();
  return tops.some((top) => top.id === chosen) ? chosen : (tops[0]?.id ?? null);
}

/** Shows another top-level area's tab. */
export function chooseTop(areaId: string): void {
  setChosenTop(areaId);
}

/** The names from the top down to and including `place`: `Tầng 2 › VIP › Phòng 1`. */
export function breadcrumb(place: AreaPlace): string {
  return [...place.path.map((id) => derived.names().get(id) ?? ""), place.name].filter((name) => name !== "").join(" › ");
}

/**
 * The sections the floor draws, from the store's `flat` sections and its `tables`. Not nested: the
 * flat sections, as before. Nested: the areas under the tab shown, each headed by its breadcrumb, an
 * area with no tables of its own drawing nothing.
 */
export function floorSections(flat: readonly FloorArea[], tables: readonly TableCard[]): readonly FloorArea[] {
  if (!derived.nested()) {
    return flat;
  }
  const top = activeTop();
  const cards = new Map<string, TableCard>(tables.map((table) => [table.id, table]));
  return places()
    .filter((place) => topOf(place) === top)
    .map((place) => ({
      name: breadcrumb(place),
      tables: place.tableIds.map((id) => cards.get(id)).filter((card): card is TableCard => card !== undefined),
    }))
    .filter((section) => section.tables.length > 0);
}
