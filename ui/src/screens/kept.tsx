// The floor and a table's order screen stay mounted (ADR-0208): an operator goes between the two
// all service long, and building the order screen again on every tap was most of what a switch cost
// at a 2× throttle. Both are drawn once, and a switch only shows one and hides the other.
//
// # What a kept order screen promises
//
// - **It shows one table, the one the address names.** While hidden it holds the last table it
//   showed, and nothing on it can be tapped (`hidden` takes it out of layout and hit testing).
// - **Nothing of one table shows on another.** Its panels, typed text and choices are reset when it
//   hides and when its table changes. A refusal or a result is kept with the table its act was for
//   (`createTableScoped`, `../state/kept-table.ts`), and a figure the edge priced is shown only for
//   the table it priced.
// - **It writes nothing on its own.** A write leaves only from a tap, and a hidden screen takes none;
//   a write already in flight goes on as it did when the screen was unmounted. A hidden screen does
//   not read the edge either: the check is read again when it shows.
// - **It is built before the first table is opened**, once the floor is drawn and the till is idle,
//   so the first seat of the day does not pay for building it either.
//
// The walk-in order (`/order/:id`) and the pay screen keep the router's own mounting.

import { Show, createMemo, createSignal, onCleanup, onMount, type JSX } from "solid-js";
import { useLocation, useParams } from "@solidjs/router";

import { KeptTableContext, type KeptTable } from "../state/kept-table";
import { Floor } from "./Floor";
import { Order } from "./Order";

const TABLE_PATH = /^\/table\/[^/]+$/;

/** How long the till waits for an idle moment before building the order screen anyway. */
const BUILD_WITHIN_MS = 1_000;

/** Runs `build` once the till is idle, or within [`BUILD_WITHIN_MS`]; cancelled with the screen. */
function whenIdle(build: () => void): void {
  if (typeof requestIdleCallback === "function") {
    const handle = requestIdleCallback(build, { timeout: BUILD_WITHIN_MS });
    onCleanup(() => cancelIdleCallback(handle));
  } else {
    const handle = setTimeout(build, 0);
    onCleanup(() => clearTimeout(handle));
  }
}

/** The floor (`/`) and a table's order screen (`/table/:id`), both kept mounted. */
export function TableScreens(): JSX.Element {
  const location = useLocation();
  const params = useParams<{ id?: string }>();
  const onOrder = () => TABLE_PATH.test(location.pathname);
  // The table shown, kept while the floor is: the order screen keeps the last table it showed.
  const held = createMemo<string>((last) => (onOrder() ? (params.id ?? last) : last), "");
  const [idle, setIdle] = createSignal(false);
  onMount(() => whenIdle(() => setIdle(true)));
  const kept: KeptTable = {
    get id() {
      return held();
    },
    shown: onOrder,
  };
  return (
    <>
      <div hidden={onOrder()}>
        <Floor />
      </div>
      <Show when={idle() || held() !== ""}>
        <div hidden={!onOrder()}>
          <KeptTableContext.Provider value={kept}>
            <Order />
          </KeptTableContext.Provider>
        </div>
      </Show>
    </>
  );
}
