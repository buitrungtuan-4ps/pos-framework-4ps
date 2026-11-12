// The floor's areas as the store published them (ADR-0211): a store whose areas are all at the top
// draws exactly what it drew before, and a nested one shows its top-level areas as tabs and the
// levels below as breadcrumb-headed sections, read from the published order and path.

import { afterEach, describe, expect, it } from "vitest";
import { render } from "solid-js/web";

import { AreaTabs } from "../src/components/AreaTabs";
import { activeTop, chooseTop, floorIsNested, floorSections, topAreas } from "../src/state/floor-tree";
import { applyFloor, floorAreas, floorTables } from "../src/state/store";

const table = (table_id: string, label: string) => ({ table_id, label, seats: 4 });

/** A plan as the edge serves it, with `areas` in the order the cloud published them. */
const plan = (areas: readonly object[]) => ({ floor: { areas }, stations: { stations: [] }, table_states: {} }) as never;

const FLAT = plan([
  { area_id: "A1", name: "Terrace", tables: [table("T1", "1"), table("T2", "2")] },
  { area_id: "A2", name: "Hall", tables: [table("T3", "3")] },
]);

/** Tầng 1 (G) with its own table; Tầng 2 (U) ⊃ VIP (V) ⊃ Phòng 1 (R), and a table on Tầng 2. */
const NESTED = plan([
  { area_id: "G", name: "Tầng 1", tables: [table("T1", "1")] },
  { area_id: "U", name: "Tầng 2", tables: [table("T2", "2")] },
  { area_id: "V", name: "VIP", parent_area_id: "U", depth: 1, path: ["U"], tables: [] },
  { area_id: "R", name: "Phòng 1", parent_area_id: "V", depth: 2, path: ["U", "V"], tables: [table("T4", "4"), table("T5", "5")] },
]);

const sections = () => floorSections(floorAreas(), floorTables()).map((section) => [section.name, section.tables.map((card) => card.label)]);

afterEach(() => {
  document.body.innerHTML = "";
});

describe("the floor's areas", () => {
  it("draw a store whose areas are all at the top exactly as before, with no tabs", () => {
    applyFloor(FLAT);
    expect(floorIsNested()).toBe(false);
    const flat = floorAreas();
    expect(floorSections(flat, floorTables())).toBe(flat);
    expect(sections()).toEqual([
      ["Terrace", ["1", "2"]],
      ["Hall", ["3"]],
    ]);
    const target = document.createElement("div");
    document.body.append(target);
    const dispose = render(() => AreaTabs(), target);
    expect(target.querySelector("[role=tablist]")).toBeNull();
    dispose();
  });

  it("show a nested store's floors as tabs, and the levels below as breadcrumbs with their tables", () => {
    applyFloor(NESTED);
    expect(floorIsNested()).toBe(true);
    expect(topAreas().map((top) => top.name)).toEqual(["Tầng 1", "Tầng 2"]);
    expect(activeTop()).toBe("G");
    expect(sections()).toEqual([["Tầng 1", ["1"]]]);

    const target = document.createElement("div");
    document.body.append(target);
    const dispose = render(() => AreaTabs(), target);
    const tabs = [...target.querySelectorAll<HTMLButtonElement>("[role=tab]")];
    expect(tabs.map((tab) => [tab.textContent, tab.getAttribute("aria-selected")])).toEqual([
      ["Tầng 1", "true"],
      ["Tầng 2", "false"],
    ]);
    tabs[1]?.click();
    expect(activeTop()).toBe("U");
    expect(tabs[1]?.getAttribute("aria-selected")).toBe("true");
    // VIP has no table of its own, so it draws nothing; its room is named by the whole path.
    expect(sections()).toEqual([
      ["Tầng 2", ["2"]],
      ["Tầng 2 › VIP › Phòng 1", ["4", "5"]],
    ]);
    dispose();
  });

  it("keep the chosen floor while it is published, and fall back to the first when it is not", () => {
    applyFloor(NESTED);
    chooseTop("U");
    applyFloor(NESTED);
    expect(activeTop()).toBe("U");
    chooseTop("GONE");
    expect(activeTop()).toBe("G");
  });
});
