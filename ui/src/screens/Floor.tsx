import { For, Show, createContext, createSignal, onCleanup, useContext, type Accessor, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { t } from "../i18n";
import { tableStateKey } from "../i18n/labels";
import { can } from "../state/permissions";
import { seatAtOnce, seatRefusal, seating, setSeatRefusal } from "../state/optimistic";
import {
  DINE_IN,
  areaIsPlaced,
  floorAreas,
  floorTables,
  seatedAt,
  tableState,
  tablesEnabled,
  type FloorArea,
  type TableCard,
} from "../state/store";
import { clean } from "../state/floor-writes";
import { floorSections } from "../state/floor-tree";
import { AreaTabs } from "../components/AreaTabs";
import { guestStep } from "../state/flows";
import { Takeaway } from "./Takeaway";
import { errorMessage } from "../lib/errors";
import { NO_ACTS, PackageScreen } from "../package/screen";

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
// How long a table has been seated is worth showing only while somebody is sitting there — to be
// served, or to pay. A table waiting to be cleaned has been left.
const SEATED_STATES = new Set(["TABLE_STATE_OCCUPIED", "TABLE_STATE_AWAITING_PAYMENT"]);

// A free table is seated and its order opened at once (ADR-0188): the Order screen opens before the
// edge answers, and a refusal brings the server back here, if they are still on that table.
function seatThenOpen(tableId: string, navigate: (to: string) => void, guestCount?: number): void {
  const order = `/table/${tableId}`;
  seatAtOnce(
    tableId,
    () => {
      if (window.location.pathname === order) {
        navigate("/");
      }
    },
    guestCount,
  );
  navigate(order);
}

/** The counts the guest pad offers, one tap each. */
const GUEST_COUNTS = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12] as const;

/** A free table the operator tapped, waiting for its guest count. */
interface AskingGuests {
  readonly tableId: string;
  readonly label: string;
  readonly required: boolean;
}

/** The free table whose guest count is being asked, and the tap that seats it with its answer. */
function createGuestAsk(navigate: (to: string) => void) {
  const [asking, setAsking] = createSignal<AskingGuests | null>(null);
  const seatAsked = (count?: number) => {
    const table = asking();
    setAsking(null);
    if (table !== null) {
      seatThenOpen(table.tableId, navigate, count);
    }
  };
  return { asking, setAsking, seatAsked };
}

// The guest count, where the package's flow makes it a step of seating a dine-in table (ADR-0192,
// `../state/flows.ts`): one tap on the count seats the table with it and opens the order, as the
// tap on the table did before. An optional step offers Skip, which seats it with the count unknown;
// a required one does not, and the edge refuses a first line on a table seated without one.
function GuestPad(props: {
  asking: AskingGuests;
  seatWithGuests: (count: number) => void;
  skipGuests: () => void;
  cancel: () => void;
}): JSX.Element {
  return (
    <div
      class="fixed inset-x-0 bottom-0 z-20 border-t border-line bg-canvas p-4 shadow-overlay"
      role="dialog"
      aria-label={t("floor.guests_title", { label: props.asking.label })}
      data-outcome="guest-pad"
    >
      <p class="mb-3 text-lg font-semibold">{t("floor.guests_title", { label: props.asking.label })}</p>
      <Show when={props.asking.required}>
        <p class="mb-3 text-sm text-ink-muted">{t("floor.guests_required")}</p>
      </Show>
      <div class="grid grid-cols-4 gap-2 tablet:grid-cols-6">
        <For each={GUEST_COUNTS}>
          {(count) => (
            <button
              type="button"
              class="min-h-touch rounded-token border border-line bg-surface text-xl font-semibold tabular-nums"
              data-step="seatWithGuests"
              onClick={() => props.seatWithGuests(count)}
            >
              {count}
            </button>
          )}
        </For>
      </div>
      <div class="mt-3 flex gap-2">
        <Show when={!props.asking.required}>
          <button
            type="button"
            class="min-h-touch flex-1 rounded-token border border-line px-3"
            data-step="skipGuests"
            onClick={() => props.skipGuests()}
          >
            {t("floor.guests_skip")}
          </button>
        </Show>
        <button type="button" class="min-h-touch flex-1 rounded-token border border-line px-3" onClick={() => props.cancel()}>
          {t("common.cancel")}
        </button>
      </div>
    </div>
  );
}

// A seat the edge refused: its sentence, and the lines typed meanwhile, which were not sent.
function SeatRefused() {
  return (
    <Show when={seatRefusal()}>
      {(refusal) => (
        <div class="mb-4 rounded-token border border-danger px-3 py-2" role="alert" data-outcome="seat-refused">
          <p class="text-danger">{refusal().message}</p>
          <Show when={refusal().lines.length > 0}>
            <p class="mt-1 text-sm">{t("floor.seat_refused_lines", { lines: refusal().lines.join(", ") })}</p>
          </Show>
          <button
            type="button"
            class="mt-2 min-h-touch rounded-token border border-line px-3 text-sm"
            onClick={() => setSeatRefusal(null)}
          >
            {t("floor.seat_refused_dismiss")}
          </button>
        </div>
      )}
    </Show>
  );
}

// What the floor's components read (ADR-0203 decision 1): the floor screen's own state, which the
// screen holds and the package's `floor_alerts` and `floor_plan` show.
interface FloorView {
  readonly error: Accessor<string | null>;
  readonly setError: (message: string | null) => void;
  readonly seatedMinutes: (tableId: string) => number | null;
}

const FloorContext = createContext<FloorView>();

// `floor_alerts`: the last tap's refusal, and a seat the edge refused. Like every component that
// reads a screen's context, it draws nothing on a screen that has none.
export function FloorAlerts(): JSX.Element {
  const view = useContext(FloorContext);
  if (view === undefined) {
    return null;
  }
  return (
    <>
      <Show when={view.error()}>
        {(message) => (
          <p class="mb-4 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
            {message()}
          </p>
        )}
      </Show>
      <SeatRefused />
    </>
  );
}

// `floor_plan`: every table by area. A free table seats and opens, a used one opens, one needing
// cleaning clears.
export function FloorPlan(): JSX.Element {
  const navigate = useNavigate();
  const view = useContext(FloorContext);
  if (view === undefined) {
    return null;
  }

  const { asking, setAsking, seatAsked } = createGuestAsk(navigate);
  const onCard = async (tableId: string, label: string) => {
    view.setError(null);
    const current = tableState(tableId);
    try {
      if (current === "TABLE_STATE_FREE") {
        const step = guestStep(DINE_IN);
        if (step === undefined) {
          seatThenOpen(tableId, navigate);
        } else {
          setAsking({ tableId, label, required: step.required });
        }
      } else if (current === "TABLE_STATE_NEEDS_CLEANING") {
        await clean(tableId);
      } else {
        navigate(`/table/${tableId}`);
      }
    } catch (caught) {
      view.setError(errorMessage(caught));
    }
  };
  // One table's card. Identical in both layouts — only where it is placed differs, so a server's
  // target never changes size or shape between a placed area and an unplaced one.
  //
  // A free table's tap seats it and a used one's clears it, which need `sales.table.manage`; a
  // person without it still sees the room, and opens the tables that are being served (ADR-0158).
  const card = (table: TableCard) => {
    const currentState = () => tableState(table.id);
    const seatsOrClears = () =>
      currentState() === "TABLE_STATE_FREE" || currentState() === "TABLE_STATE_NEEDS_CLEANING";
    return (
      <button
        type="button"
        class="flex min-h-touch flex-col items-start gap-2 rounded-token border border-line bg-surface p-4 text-left disabled:opacity-50 tablet:[grid-column:var(--table-column,auto)] tablet:[grid-row:var(--table-row,auto)]"
        disabled={seatsOrClears() && !can("sales.table.manage")}
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
        onClick={() => void onCard(table.id, table.label)}
      >
        <span class="text-xl font-semibold">{t("common.table", { label: table.label })}</span>
        <span class="inline-flex items-center gap-2 text-sm text-ink-muted">
          <span
            class={`inline-block h-2.5 w-2.5 rounded-full ${DOT[currentState()] ?? "bg-free"}`}
            aria-hidden="true"
          />
          {t(seating(table.id) ? "floor.seating" : tableStateKey(currentState()))}
        </span>
        {/* Shown only where the store recorded a capacity: zero means "not recorded" on the wire
            (`pos_proto::floor::FloorTable`), and printing "seats 0" would be a lie a host acts on. */}
        <Show when={table.seats > 0}>
          <span class="text-sm text-ink-muted">{t("floor.seats", { count: table.seats })}</span>
        </Show>
        {/* How long the guests have been sitting, which a host reads to know who is due a check and
            who is about to leave. From when the order opened, as the edge recorded it, and from the
            first whole minute: "seated 0 min" on a table sat down a moment ago says nothing. */}
        <Show when={view.seatedMinutes(table.id)}>
          {(minutes) => (
            <span class="text-sm tabular-nums text-ink-muted" data-outcome="table-seated">
              {t("floor.seated_for", { minutes: minutes() })}
            </span>
          )}
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

  // The floor drawn is what proves a sign-in took, so it carries the outcome mark for that task
  // (ADR-0109). It is present whenever this screen renders, which is the claim.
  return (
    <div data-outcome="floor">
      <AreaTabs />
      <For each={floorSections(floorAreas(), floorTables())}>{area}</For>
      <Show when={asking()}>
        {(table) => <GuestPad asking={table()} seatWithGuests={seatAsked} skipGuests={() => seatAsked()} cancel={() => setAsking(null)} />}
      </Show>
    </div>
  );
}

export function Floor() {
  const [error, setError] = createSignal<string | null>(null);
  // The floor's clock, for how long each table has been sitting. Every thirty seconds: the figure
  // is in whole minutes, and one timer serves every card.
  const [now, setNow] = createSignal(Date.now());
  const clock = setInterval(() => setNow(Date.now()), 30_000);
  onCleanup(() => clearInterval(clock));
  // Whole minutes since the guests sat down, or null when nobody is sitting there. A device clock
  // behind the edge's would read a fresh table as seated in the future; that is zero minutes.
  const seatedMinutes = (tableId: string) => {
    const since = seatedAt(tableId);
    if (since === undefined || !SEATED_STATES.has(tableState(tableId))) {
      return null;
    }
    const started = Date.parse(since);
    return Number.isNaN(started) ? null : Math.max(0, Math.floor((now() - started) / 60_000));
  };

  // A shop with no tables gets the counter list, which is home for that role (ADR-0093). Checked
  // before anything else this screen does, so a counter store never renders a room it has not got.
  if (!tablesEnabled()) {
    return <Takeaway />;
  }

  // Drawn from the default package's `floor` screen (ADR-0190): its frame and heading, then
  // `floor_alerts` and `floor_plan` above, which read this screen's state.
  return (
    <FloorContext.Provider value={{ error, setError, seatedMinutes }}>
      <PackageScreen screenId="floor" host={NO_ACTS} />
    </FloorContext.Provider>
  );
}
