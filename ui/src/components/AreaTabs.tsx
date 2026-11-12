// The floor's top-level areas as tabs (ADR-0211): a store on several floors shows one at a time, so a
// server upstairs does not scroll past the ground floor. Drawn only for a nested store with more than
// one top-level area; a store whose areas are all at the top keeps the screen it always had.

import { For, Show, type JSX } from "solid-js";

import { t } from "../i18n";
import { activeTop, chooseTop, floorIsNested, topAreas } from "../state/floor-tree";

export function AreaTabs(): JSX.Element {
  return (
    <Show when={floorIsNested() && topAreas().length > 1}>
      <div class="mb-4 flex gap-2 overflow-x-auto" role="tablist" aria-label={t("floor.area_tabs")} data-outcome="area-tabs">
        <For each={topAreas()}>
          {(top) => (
            <button
              type="button"
              role="tab"
              aria-selected={activeTop() === top.id}
              class="min-h-touch shrink-0 rounded-token border border-line px-4 aria-selected:border-accent aria-selected:bg-surface aria-selected:font-semibold"
              data-area-tab={top.id}
              onClick={() => chooseTop(top.id)}
            >
              {top.name}
            </button>
          )}
        </For>
      </div>
    </Show>
  );
}
