// The drag-and-drop floor plan (roadmap-v3 B8.2): a table dropped on an empty cell, into another
// area or onto "Not placed" is moved there; a cell another table holds refuses it; and the arrow
// keys move a focused table one cell, so the pointer is never the only way.

import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Area, FloorTable } from "../src/api/schemas";
import { canPlace, gridSize, occupant, step } from "../src/lib/floor-grid";
import { PlanView } from "../src/screens/floor/PlanView";

const area = (area_id: string, name: string) => ({ area_id, name, status: "active" }) as unknown as Area;
const table = (table_id: string, area_id: string, position: { column: number; row: number } | null) =>
  ({ table_id, area_id, label: table_id, seats: 4, status: "active", position, etag: "v1" }) as unknown as FloorTable;

const AREAS = [area("A1", "Terrace"), area("A2", "Garden")];
const TABLES = [table("T1", "A1", { column: 0, row: 0 }), table("T2", "A1", { column: 1, row: 0 }), table("T3", "A2", null)];

/** A drop carrying `tableId`, as a drag of its card carries it. */
function drop(target: Element, tableId: string) {
  const data = { getData: () => `pos-table:${tableId}`, setData: () => undefined };
  fireEvent.dragOver(target, { dataTransfer: data });
  fireEvent.drop(target, { dataTransfer: data });
}

describe("the floor grid", () => {
  it("is drawn one cell beyond its furthest table, and a cell holds one table", () => {
    expect(gridSize(TABLES)).toEqual({ columns: 6, rows: 4 });
    expect(gridSize([table("T9", "A1", { column: 9, row: 5 })])).toEqual({ columns: 11, rows: 7 });
    expect(occupant(TABLES, { column: 1, row: 0 })?.table_id).toBe("T2");
    expect(canPlace(TABLES[0]!, TABLES, { column: 1, row: 0 })).toBe(false);
    expect(canPlace(TABLES[0]!, TABLES, { column: 0, row: 0 })).toBe(true);
    expect(canPlace(TABLES[0]!, TABLES, { column: -1, row: 0 })).toBe(false);
    expect(step({ column: 2, row: 2 }, "ArrowUp")).toEqual({ column: 2, row: 1 });
    expect(step({ column: 2, row: 2 }, "Enter")).toBeNull();
  });
});

describe("the floor plan", () => {
  afterEach(cleanup);

  function mount(canManage = true) {
    const onMove = vi.fn();
    render(() => <PlanView areas={AREAS} tables={TABLES} canManage={canManage} busy={false} onMove={onMove} />);
    return onMove;
  }

  it("moves a dropped table to an empty cell, into another area, or off the grid", () => {
    const onMove = mount();
    drop(document.querySelector('[data-cell="A1:3:2"]')!, "T1");
    expect(onMove).toHaveBeenLastCalledWith(TABLES[0], { areaId: "A1", cell: { column: 3, row: 2 } });
    drop(document.querySelector('[data-cell="A2:0:0"]')!, "T1");
    expect(onMove).toHaveBeenLastCalledWith(TABLES[0], { areaId: "A2", cell: { column: 0, row: 0 } });
    drop(document.querySelector('[data-unplaced="A1"]')!, "T2");
    expect(onMove).toHaveBeenLastCalledWith(TABLES[1], { areaId: "A1", cell: null });
  });

  it("refuses a cell another table holds, and moves nothing for a role that cannot manage", () => {
    const onMove = mount();
    drop(document.querySelector('[data-cell="A1:1:0"]')!, "T1");
    expect(onMove).not.toHaveBeenCalled();
    cleanup();
    const readOnly = mount(false);
    drop(document.querySelector('[data-cell="A1:3:2"]')!, "T1");
    expect(readOnly).not.toHaveBeenCalled();
  });

  it("moves a focused table one cell per arrow key, and not onto another table", () => {
    const onMove = mount();
    const card = document.querySelector('[data-table-id="T1"]')!;
    fireEvent.keyDown(card, { key: "ArrowDown" });
    expect(onMove).toHaveBeenLastCalledWith(TABLES[0], { areaId: "A1", cell: { column: 0, row: 1 } });
    onMove.mockClear();
    fireEvent.keyDown(card, { key: "ArrowRight" });
    expect(onMove).not.toHaveBeenCalled();
  });
});
