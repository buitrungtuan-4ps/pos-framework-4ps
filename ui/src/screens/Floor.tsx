import { For, Show, createSignal } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { PageHeader } from "../components/ui";
import { t } from "../i18n";
import { tableStateKey } from "../i18n/labels";
import {
  areaIsPlaced,
  clean,
  floorAreas,
  seat,
  tableState,
  tablesEnabled,
  type FloorArea,
  type TableCard,
} from "../state/store";
import { Takeaway } from "./Takeaway";
import { errorMessage } from "../lib/errors";

const DOT: Record<string, string> = {
  TABLE_STATE_FREE: "bg-free",
  TABLE_STATE_OCCUPIED: "bg-occupied",
  TABLE_STATE_AWAITING_PAYMENT: "bg-awaiting",
  TABLE_STATE_NEEDS_CLEANING: "bg-cleaning",
};

// Home — which is a floor plan in most shops and not in all of them.
//
// One card per table, its state shown by a labelled colour (never colour alone). A free table seats
// and opens; an occupied or paying table opens its order; a table needing cleaning offers the one
// action that clears it.
//
// # Why the counter list is here and not behind a route of its own
//
// `docs/ui-ux.md` §3: *"The store profile decides the starting screen and flow... Same components,
// different assembly — not three applications."* §10's capability model has carried that since it
// was written — there is a counter preset, a retail preset, and a rule saying `PayFirst` and
// `Tables` cannot both be on — and the till implemented one profile: every store landed on a floor
// plan, including the ones with no floor.
//
// So `/` is still `/`, and what it draws follows `tables_enabled`. The choice sits in this file
// rather than in a component `App.tsx` composes, because `scripts/step-budget.mjs` resolves a route
// to `screens/<Name>.tsx` and scans **that file**: a wrapper that merely picked between two screens
// would hide both of them from the gate, and the flows declared on `/` would stop resolving. Making
// home a different *address* per profile was the other option and is worse — it would make "home" a
// route that depends on the shop, which is the opposite of one application assembled differently.
//
// **Retail is not here.** §1 principle 9's third profile starts on a barcode field, and no such
// screen exists — `Capability::Barcode` has no reader anywhere in the till. A retail store lands on
// the counter with everything else that is not table service, and that gap is recorded rather than
// guessed at.
//
// **It is drawn the way the store published it** (ADR-0072). Until now this screen flattened every
// area into one reflowing grid in publication order, throwing away the three things the console had
// gone to the trouble of recording: which area a table is in, how many it seats, and where it sits
// in the floor editor's grid. A server hunting "the six-top on the terrace" had to read every label
// on a board that matched no room.
//
// Two layouts, chosen per area rather than per store, because a store can place its dining room and
// leave the bar unplaced:
//
//   * **Placed** — every table in the area has a position, so it is drawn on a CSS grid at those
//     column/row coordinates. The screen is then a picture of the room, and pointing works.
//   * **Unplaced** — no grid to honour, so the cards reflow as before. This is also the fallback
//     floor's shape, and a phone's.
//
// Nothing here fails when the plan is thin: an area with no name loses its heading, a table with no
// seat count shows no capacity, and a store with neither is exactly the screen that shipped before.
export function Floor() {
  const navigate = useNavigate();
  const [error, setError] = createSignal<string | null>(null);

  const onCard = async (tableId: string) => {
    setError(null);
    const current = tableState(tableId);
    try {
      if (current === "TABLE_STATE_FREE") {
        await seat(tableId);
        navigate(`/table/${tableId}`);
      } else if (current === "TABLE_STATE_NEEDS_CLEANING") {
        await clean(tableId);
      } else {
        navigate(`/table/${tableId}`);
      }
    } catch (caught) {
      setError(errorMessage(caught));
    }
  };

  // One table's card. Identical in both layouts — only where it is placed differs, so a server's
  // target never changes size or shape between a placed area and an unplaced one.
  const card = (table: TableCard) => {
    const currentState = () => tableState(table.id);
    return (
      <button
        type="button"
        class="flex min-h-touch flex-col items-start gap-2 rounded-token border border-line bg-surface p-4 text-left tablet:[grid-column:var(--table-column,auto)] tablet:[grid-row:var(--table-row,auto)]"
        // `pos_proto::display::GridPosition` counts from zero; CSS grid lines count from one. The
        // `+ 1` is that conversion and nothing else — without it every table shifts up and left, and
        // the table the editor put in the top-left corner lands on line 0, which CSS ignores.
        //
        // Carried as custom properties and applied only from a tablet up (F7): on a phone a placed
        // room is a pan across columns at least 9rem wide, two and a half tables to a screen, so a
        // phone reflows it into two columns — the shape this screen's header comment always said a
        // phone gets.
        style={
          table.position
            ? {
                "--table-column": String(table.position.column + 1),
                "--table-row": String(table.position.row + 1),
              }
            : undefined
        }
        data-step="onCard"
        onClick={() => void onCard(table.id)}
      >
        <span class="text-xl font-semibold">{t("common.table", { label: table.label })}</span>
        <span class="inline-flex items-center gap-2 text-sm text-ink-muted">
          <span
            class={`inline-block h-2.5 w-2.5 rounded-full ${DOT[currentState()] ?? "bg-free"}`}
            aria-hidden="true"
          />
          {t(tableStateKey(currentState()))}
        </span>
        {/* Shown only where the store recorded a capacity: zero means "not recorded" on the wire
            (`pos_proto::floor::FloorTable`), and printing "seats 0" would be a lie a host acts on. */}
        <Show when={table.seats > 0}>
          <span class="text-sm text-ink-muted">{t("floor.seats", { count: table.seats })}</span>
        </Show>
      </button>
    );
  };

  // A placed area is drawn on the editor's own grid: `grid-auto-columns` keeps every column the same
  // width whether or not a table sits in it, so a gap in the room stays a gap on the screen. An
  // unplaced one reflows, which is what this screen has always done and what a phone needs anyway.
  const area = (group: FloorArea) => (
    <section class="mb-6 last:mb-0">
      <Show when={group.name !== ""}>
        <h2 class="mb-2 text-sm font-semibold uppercase tracking-wide text-ink-muted">
          {group.name}
        </h2>
      </Show>
      <div
        class={
          areaIsPlaced(group)
            ? "grid grid-cols-2 gap-3 tablet:grid-cols-none tablet:overflow-x-auto tablet:[grid-auto-columns:minmax(9rem,1fr)] tablet:[grid-auto-flow:dense]"
            : "grid grid-cols-2 gap-3 tablet:grid-cols-3 terminal:grid-cols-4"
        }
      >
        <For each={group.tables}>{card}</For>
      </div>
    </section>
  );

  // A shop with no tables gets the counter list, which is home for that role (ADR-0093). Checked
  // before anything else this screen does, so a counter store never renders a room it has not got.
  if (!tablesEnabled()) {
    return <Takeaway />;
  }

  return (
    <section class="p-4">
      <PageHeader title={t("floor.title")} />
      <Show when={error()}>
        {(message) => (
          <p class="mb-4 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
            {message()}
          </p>
        )}
      </Show>
      {/* The floor drawn is what proves a sign-in took, so it carries the outcome mark for that
          task (ADR-0109). It is present whenever this screen renders, which is the claim. */}
      <div data-outcome="floor">
        <For each={floorAreas()}>{area}</For>
      </div>
    </section>
  );
}
