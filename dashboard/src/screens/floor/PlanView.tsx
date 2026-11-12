// The floor plan as a picture (roadmap-v3 B8.2, ADR-0072): each area a grid, each table a card in
// its cell, dragged to another cell, into another area, or off the grid to unplace it. Native HTML
// drag and drop, no library. A focused card moves one cell per arrow key, and the table list beside
// this view stays the form for everything else, so the pointer is never the only way (D17).
//
// A move writes the table's `GridPosition` (and its area) through the table update route, with the
// version it was read at; publishing the floor still takes the plan to the store.

import { For, Show } from "solid-js";

import type { Area, FloorTable } from "../../api/schemas";
import { t } from "../../i18n";
import { canPlace, gridSize, occupant, step, tablesOf, type Cell } from "../../lib/floor-grid";

/** Where a table goes: a cell of an area, or off the grid. */
export type Placement = { readonly areaId: string; readonly cell: Cell | null };

type PlanProps = {
  readonly areas: readonly Area[];
  readonly tables: readonly FloorTable[];
  readonly canManage: boolean;
  readonly busy: boolean;
  readonly onMove: (table: FloorTable, placement: Placement) => void;
};

const DRAG_PREFIX = "pos-table:";

function draggedTable(event: DragEvent, tables: readonly FloorTable[]): FloorTable | undefined {
  const text = event.dataTransfer?.getData("text/plain") ?? "";
  return text.startsWith(DRAG_PREFIX) ? tables.find((table) => table.table_id === text.slice(DRAG_PREFIX.length)) : undefined;
}

/** A table's card: dragged anywhere, or moved by the arrow keys while it has focus. */
function TableCard(props: PlanProps & { table: FloorTable; inArea: readonly FloorTable[] }) {
  return (
    <div
      role="button"
      tabindex={props.canManage ? 0 : -1}
      draggable={props.canManage && !props.busy}
      aria-label={t("floor.planTable", { label: props.table.label, seats: props.table.seats })}
      aria-keyshortcuts="ArrowUp ArrowDown ArrowLeft ArrowRight"
      class="flex h-full cursor-grab flex-col items-center justify-center rounded-token border border-line bg-surface-raised text-sm"
      data-table-id={props.table.table_id}
      onDragStart={(event) => event.dataTransfer?.setData("text/plain", `${DRAG_PREFIX}${props.table.table_id}`)}
      onKeyDown={(event) => {
        const from = props.table.position;
        const next = from ? step(from, event.key) : null;
        if (next && props.canManage && !props.busy) {
          event.preventDefault();
          if (canPlace(props.table, props.inArea, next)) {
            props.onMove(props.table, { areaId: props.table.area_id, cell: next });
          }
        }
      }}
    >
      <span class="font-medium text-ink">{props.table.label}</span>
      <span class="text-ink-muted">{t("floor.planSeats", { seats: props.table.seats })}</span>
    </div>
  );
}

/** A drop target: a cell of an area, or the unplaced tray. */
function dropHandlers(props: PlanProps, placement: Placement, inArea: readonly FloorTable[]) {
  return {
    onDragOver: (event: DragEvent) => event.preventDefault(),
    onDrop: (event: DragEvent) => {
      event.preventDefault();
      const table = draggedTable(event, props.tables);
      if (table && props.canManage && !props.busy && (placement.cell === null || canPlace(table, inArea, placement.cell))) {
        props.onMove(table, placement);
      }
    },
  };
}

/** One area's grid. */
function AreaGrid(props: PlanProps & { area: Area }) {
  const inArea = () => tablesOf(props.tables, props.area.area_id);
  const size = () => gridSize(inArea());
  const cells = () =>
    Array.from({ length: size().rows * size().columns }, (_, at) => ({
      column: at % size().columns,
      row: Math.floor(at / size().columns),
    }));
  const unplaced = () => inArea().filter((table) => !table.position);
  return (
    <section class="flex flex-col gap-2" aria-label={props.area.name}>
      <h3 class="text-sm font-semibold text-ink">{props.area.name}</h3>
      <div class="grid gap-1 overflow-x-auto" style={{ "grid-template-columns": `repeat(${size().columns}, minmax(4.5rem, 1fr))` }}>
        <For each={cells()}>
          {(cell) => (
            <div
              class="min-h-16 rounded-token border border-dashed border-line"
              data-cell={`${props.area.area_id}:${cell.column}:${cell.row}`}
              {...dropHandlers(props, { areaId: props.area.area_id, cell }, inArea())}
            >
              <Show when={occupant(inArea(), cell)}>
                {(table) => <TableCard {...props} table={table()} inArea={inArea()} />}
              </Show>
            </div>
          )}
        </For>
      </div>
      <div
        class="flex min-h-12 flex-wrap items-center gap-2 rounded-token border border-dashed border-line p-2"
        data-unplaced={props.area.area_id}
        {...dropHandlers(props, { areaId: props.area.area_id, cell: null }, inArea())}
      >
        <span class="text-sm text-ink-muted">{t("floor.planUnplaced")}</span>
        <For each={unplaced()}>
          {(table) => (
            <div class="h-14 w-20">
              <TableCard {...props} table={table} inArea={inArea()} />
            </div>
          )}
        </For>
      </div>
    </section>
  );
}

/** Every active area's grid. */
export function PlanView(props: PlanProps) {
  const active = () => props.areas.filter((area) => area.status !== "archived");
  return (
    <div class="flex flex-col gap-6">
      <p class="text-sm text-ink-muted">{t("floor.planHint")}</p>
      <For each={active()}>{(area) => <AreaGrid {...props} area={area} />}</For>
    </div>
  );
}
