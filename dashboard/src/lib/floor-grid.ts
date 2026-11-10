// The floor plan's grid (ADR-0072, roadmap-v3 B8.2): each area is a grid of cells, and a table sits
// in one cell (`GridPosition`, column and row from 0) or in none, unplaced. The designer drags a
// table onto a cell; these functions say how big an area's grid is drawn, who sits where, and
// whether a move is allowed. A cell holds one table.

import type { FloorTable } from "../api/schemas";

/** A cell of an area's grid. */
export type Cell = { readonly column: number; readonly row: number };

/** The least an area's grid is drawn, so an empty area still has room to drop into. */
export const GRID_MIN = { columns: 6, rows: 4 } as const;

/** The most an area's grid grows to as tables are placed further out. */
export const GRID_MAX = { columns: 24, rows: 24 } as const;

/** The tables of `areaId` that are not archived. */
export function tablesOf(tables: readonly FloorTable[], areaId: string): FloorTable[] {
  return tables.filter((table) => table.area_id === areaId && table.status !== "archived");
}

/** How many columns and rows an area's grid is drawn with: one spare beyond its furthest table. */
export function gridSize(tables: readonly FloorTable[]): { columns: number; rows: number } {
  let columns: number = GRID_MIN.columns;
  let rows: number = GRID_MIN.rows;
  for (const table of tables) {
    if (table.position) {
      columns = Math.max(columns, table.position.column + 2);
      rows = Math.max(rows, table.position.row + 2);
    }
  }
  return { columns: Math.min(columns, GRID_MAX.columns), rows: Math.min(rows, GRID_MAX.rows) };
}

/** The table in `cell` of an area, if any. */
export function occupant(tables: readonly FloorTable[], cell: Cell): FloorTable | undefined {
  return tables.find((table) => table.position?.column === cell.column && table.position.row === cell.row);
}

/**
 * Whether `table` may move to `cell` of the area whose tables are `inArea`: the cell is on the
 * grid, and empty or the table's own.
 */
export function canPlace(table: FloorTable, inArea: readonly FloorTable[], cell: Cell): boolean {
  if (cell.column < 0 || cell.row < 0 || cell.column >= GRID_MAX.columns || cell.row >= GRID_MAX.rows) {
    return false;
  }
  const there = occupant(inArea, cell);
  return there === undefined || there.table_id === table.table_id;
}

/** The cell one step from `cell` toward an arrow key, or `null` for any other key. */
export function step(cell: Cell, key: string): Cell | null {
  switch (key) {
    case "ArrowLeft":
      return { column: cell.column - 1, row: cell.row };
    case "ArrowRight":
      return { column: cell.column + 1, row: cell.row };
    case "ArrowUp":
      return { column: cell.column, row: cell.row - 1 };
    case "ArrowDown":
      return { column: cell.column, row: cell.row + 1 };
    default:
      return null;
  }
}
