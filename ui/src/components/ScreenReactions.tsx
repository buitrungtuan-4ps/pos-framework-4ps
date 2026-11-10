// The router's half of the package's reactions (`../state/reactions.ts`): a table's order screen
// entered fires `REACTION_TRIGGER_SCREEN_ENTERED` for the package screen `order`, and a reaction
// whose act is `ui.screen.open` opens a package screen through the router, as does a package
// screen's button bound to it (`../package/screen.tsx`). Draws nothing.

import { createEffect, on } from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";

import { tableReacts } from "../state/optimistic";
import { setScreenOpener } from "../state/reactions";
import { SCREENS } from "../state/screens";

/** The package's screens a reaction may open, by the route each is drawn at. */
const ROUTES: Readonly<Record<string, string>> = {
  floor: SCREENS.floor.path,
  order: SCREENS.tableOrder.path,
  pay: SCREENS.tablePay.path,
  kds: SCREENS.kds.path,
  expo: SCREENS.expo.path,
  guests: SCREENS.guests.path,
  counter: SCREENS.counter.path,
  today: SCREENS.today.path,
  records: SCREENS.records.path,
  shift: SCREENS.shift.path,
};

const TABLE_ORDER = /^\/table\/([^/]+)$/;

export function ScreenReactions() {
  const navigate = useNavigate();
  const location = useLocation();
  setScreenOpener((screenId, fired) => {
    const route = ROUTES[screenId];
    const path = route?.includes(":id") === true ? (fired.tableId === undefined ? undefined : route.replace(":id", fired.tableId)) : route;
    if (path === undefined) {
      return false;
    }
    navigate(path);
    return true;
  });
  createEffect(
    on(
      () => location.pathname,
      (pathname) => {
        const table = TABLE_ORDER.exec(pathname)?.[1];
        if (table !== undefined) {
          tableReacts("REACTION_TRIGGER_SCREEN_ENTERED", table, { screenId: "order" });
        }
      },
    ),
  );
  return null;
}
