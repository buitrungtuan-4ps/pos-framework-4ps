// The tree of a store's areas (ADR-0211): an area may sit inside another, a floor being simply a
// top-level area. The cloud keeps the tree honest — it refuses a cycle, a branch deeper than eight
// levels, and archiving an area whose children are live — and these functions only let the console
// draw it, offer the moves the cloud would take, and warn (never refuse) past three levels.
//
// Order is the one the cloud publishes and the till draws: the top level by id, each area followed by
// the areas inside it, also by id. An id is a ULID, so that is the order the areas were added in.

import type { Area } from "../api/schemas";

/** The most levels a branch may hold, the top counting as one (`MAX_AREA_LEVELS` in pos-proto). */
export const MAX_AREA_LEVELS = 8;

/** Past this many levels the console warns that a plan is deep (`WARN_AREA_LEVELS`). */
export const WARN_AREA_LEVELS = 3;

/** One area in tree order: how deep it sits (0 at the top) and the areas it sits inside, from the top. */
export type AreaNode = { readonly area: Area; readonly depth: number; readonly path: readonly Area[] };

const byId = (a: Area, b: Area) => (a.area_id < b.area_id ? -1 : a.area_id > b.area_id ? 1 : 0);

/**
 * `areas` in tree order. An area whose parent is not among `areas` is drawn at the top, as the cloud
 * publishes it, and an area is never drawn twice, so a broken store still draws every area.
 */
export function areaTree(areas: readonly Area[]): AreaNode[] {
  const known = new Map(areas.map((area) => [area.area_id, area]));
  const children = new Map<string, Area[]>();
  for (const area of areas) {
    const parent = area.parent_area_id;
    if (parent && known.has(parent) && parent !== area.area_id) {
      children.set(parent, [...(children.get(parent) ?? []), area]);
    }
  }
  const out: AreaNode[] = [];
  const seen = new Set<string>();
  const visit = (area: Area, path: readonly Area[]) => {
    if (seen.has(area.area_id)) {
      return;
    }
    seen.add(area.area_id);
    out.push({ area, depth: path.length, path });
    for (const child of [...(children.get(area.area_id) ?? [])].sort(byId)) {
      visit(child, [...path, area]);
    }
  };
  const sorted = [...areas].sort(byId);
  for (const area of sorted) {
    const parent = area.parent_area_id;
    if (!parent || !known.has(parent)) {
      visit(area, []);
    }
  }
  // Whatever a cycle kept from the top is drawn at the top too.
  for (const area of sorted) {
    visit(area, []);
  }
  return out;
}

/** The ids of every area inside `areaId`, at any depth. */
export function descendants(areas: readonly Area[], areaId: string): Set<string> {
  const inside = new Set<string>();
  let frontier = [areaId];
  while (frontier.length > 0) {
    const next = areas
      .filter((area) => area.parent_area_id && frontier.includes(area.parent_area_id) && !inside.has(area.area_id))
      .map((area) => area.area_id);
    for (const id of next) {
      inside.add(id);
    }
    frontier = next.filter((id) => id !== areaId);
  }
  inside.delete(areaId);
  return inside;
}

/** How many levels `areaId`'s branch holds below and including it (1 for an area with nothing inside). */
function branchLevels(areas: readonly Area[], areaId: string): number {
  const nodes = areaTree(areas);
  const at = nodes.find((node) => node.area.area_id === areaId);
  if (!at) {
    return 1;
  }
  const inside = descendants(areas, areaId);
  return nodes.filter((node) => inside.has(node.area.area_id)).reduce((most, node) => Math.max(most, node.depth - at.depth + 1), 1);
}

/**
 * The live areas `areaId` may move inside: not itself, not an area inside it, and not one where its
 * branch would hold more than {@link MAX_AREA_LEVELS} levels. The top is always a place it may go.
 */
export function moveTargets(areas: readonly Area[], areaId: string): Area[] {
  const inside = descendants(areas, areaId);
  const levels = branchLevels(areas, areaId);
  return areaTree(areas)
    .filter(
      (node) =>
        node.area.status === "active" &&
        node.area.area_id !== areaId &&
        !inside.has(node.area.area_id) &&
        node.depth + 1 + levels <= MAX_AREA_LEVELS,
    )
    .map((node) => node.area);
}

/** How many levels the live plan holds: 0 for none, 1 when every area is at the top. */
export function levelsOf(areas: readonly Area[]): number {
  const live = areas.filter((area) => area.status === "active");
  return areaTree(live).reduce((most, node) => Math.max(most, node.depth + 1), 0);
}

/** The names from the top down to and including `node`'s area: `Tầng 2 › VIP › Phòng 1`. */
export function breadcrumb(node: AreaNode): string {
  return [...node.path, node.area].map((area) => area.name).join(" › ");
}

/** The reason tokens the cloud refuses a tree write with, in the order the console explains them. */
export const TREE_REFUSALS = [
  "AREA_PARENT_NOT_FOUND",
  "AREA_PARENT_ARCHIVED",
  "AREA_CYCLE",
  "AREA_TOO_DEEP",
  "AREA_HAS_LIVE_CHILDREN",
] as const;

/** One of {@link TREE_REFUSALS}. */
export type TreeRefusal = (typeof TREE_REFUSALS)[number];

/** The tree refusal among a failure's field details, if it carries one. */
export function treeRefusal(details: readonly { readonly reason: string }[]): TreeRefusal | null {
  const found = details.find((detail) => (TREE_REFUSALS as readonly string[]).includes(detail.reason));
  return found ? (found.reason as TreeRefusal) : null;
}
