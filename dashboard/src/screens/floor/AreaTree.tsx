// The store's areas as a tree (ADR-0211): a floor is a top-level area, and an area may sit inside
// another. A branch moves by dragging an area onto the one it should sit inside, or onto "Top
// level"; every row also has a "Move to" list, so the pointer is never the only way (D17). The
// console offers only the moves the cloud would take, the cloud still decides, and a refusal is
// shown on the row it is about. Past three levels the plan is deep, which is a warning, never a
// refusal.

import { For, Show } from "solid-js";

import type { Area } from "../../api/schemas";
import { SelectField } from "../../components/ui";
import { t } from "../../i18n";
import { areaTree, levelsOf, moveTargets, WARN_AREA_LEVELS } from "../../lib/area-tree";

/** The top level, as a move target. */
const TOP = "";

const DRAG_PREFIX = "pos-area:";

type TreeProps = {
  readonly areas: readonly Area[];
  readonly canManage: boolean;
  readonly busy: boolean;
  /** The refusal to show on an area's row, already in the operator's words, or none. */
  readonly refusal: (areaId: string) => string | undefined;
  /** Move `area` inside `parentId`, or to the top for `null`. */
  readonly onMove: (area: Area, parentId: string | null) => void;
};

function draggedArea(event: DragEvent, areas: readonly Area[]): Area | undefined {
  const text = event.dataTransfer?.getData("text/plain") ?? "";
  return text.startsWith(DRAG_PREFIX) ? areas.find((area) => area.area_id === text.slice(DRAG_PREFIX.length)) : undefined;
}

/** Drop handlers that move the dragged area inside `parentId`, when that is a move the cloud takes. */
function dropInto(props: TreeProps, parentId: string | null) {
  return {
    onDragOver: (event: DragEvent) => event.preventDefault(),
    onDrop: (event: DragEvent) => {
      event.preventDefault();
      const area = draggedArea(event, props.areas);
      if (!area || !props.canManage || props.busy || (area.parent_area_id ?? null) === parentId) {
        return;
      }
      if (parentId === null || moveTargets(props.areas, area.area_id).some((target) => target.area_id === parentId)) {
        props.onMove(area, parentId);
      }
    },
  };
}

export function AreaTree(props: TreeProps) {
  const live = () => props.areas.filter((area) => area.status === "active");
  const nodes = () => areaTree(live());
  const levels = () => levelsOf(props.areas);
  return (
    <div class="flex flex-col gap-3">
      <p class="text-sm text-ink-muted">{t("floor.treeHint")}</p>
      <Show when={levels() > WARN_AREA_LEVELS}>
        <p role="status" class="rounded-token border border-line bg-surface-raised px-3 py-2 text-sm text-ink" data-tree-deep>
          {t("floor.treeDeep", { levels: levels(), warn: WARN_AREA_LEVELS })}
        </p>
      </Show>
      <div
        class="rounded-token border border-dashed border-line px-3 py-2 text-sm text-ink-muted"
        data-tree-top
        {...dropInto(props, null)}
      >
        {t("floor.treeTop")}
      </div>
      <ul class="flex flex-col gap-1" aria-label={t("floor.tree")}>
        <For each={nodes()}>
          {(node) => (
            <li style={{ "padding-inline-start": `${node.depth * 1.5}rem` }} data-tree-area={node.area.area_id}>
              <div
                class="flex flex-wrap items-center gap-3 rounded-token border border-line bg-surface-raised px-3 py-2"
                draggable={props.canManage && !props.busy}
                aria-label={t("floor.treeArea", { name: node.area.name, level: node.depth + 1 })}
                onDragStart={(event) => event.dataTransfer?.setData("text/plain", `${DRAG_PREFIX}${node.area.area_id}`)}
                {...dropInto(props, node.area.area_id)}
              >
                <span class="min-w-32 flex-1 font-medium text-ink">{node.area.name}</span>
                <Show when={props.canManage}>
                  <div class="w-56">
                    <SelectField
                      label={t("floor.treeMoveTo", { name: node.area.name })}
                      value={node.area.parent_area_id ?? TOP}
                      disabled={props.busy}
                      options={[
                        { value: TOP, label: t("floor.treeTop") },
                        ...moveTargets(props.areas, node.area.area_id).map((area) => ({ value: area.area_id, label: area.name })),
                      ]}
                      onChange={(value) => {
                        if (value !== (node.area.parent_area_id ?? TOP)) {
                          props.onMove(node.area, value === TOP ? null : value);
                        }
                      }}
                    />
                  </div>
                </Show>
              </div>
              <Show when={props.refusal(node.area.area_id)}>
                {(message) => (
                  <p role="alert" class="mt-1 text-sm text-danger" data-tree-refusal={node.area.area_id}>
                    {message()}
                  </p>
                )}
              </Show>
            </li>
          )}
        </For>
      </ul>
    </div>
  );
}
