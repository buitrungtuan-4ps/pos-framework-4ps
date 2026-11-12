// The router's half of the package's reactions (`../state/reactions.ts`): a table's order screen
// entered fires `REACTION_TRIGGER_SCREEN_ENTERED` for the package screen `order`, and a reaction
// whose act is `ui.screen.open` opens a package screen through the router, as does a package
// screen's button bound to it (`../package/screen.tsx`). A screen the running version adds, one
// the release has no route for, opens at `/screen/:id` (`../screens/VersionScreen.tsx`). Draws
// nothing.

import { createEffect, on } from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";

import { deviceClass } from "../lib/device-class";
import { defaultScreen } from "../package/screen";
import { tableReacts } from "../state/optimistic";
import { homeScreenId, publishedScreen } from "../state/package-screens";
import { setScreenOpener } from "../state/reactions";
import { SCREENS } from "../state/screens";
import { roleIds } from "../state/session";

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

/**
 * The route the package screen `screenId` is drawn at: its own, for one of the release's screens;
 * `/screen/:id` for one the running version adds; none for one the till cannot open.
 */
export function screenRoute(screenId: string): string | undefined {
  const own = ROUTES[screenId];
  if (own !== undefined) {
    return own;
  }
  const added = defaultScreen(screenId) === undefined && publishedScreen(screenId).length > 0;
  return added ? SCREENS.versionScreen.path.replace(":id", screenId) : undefined;
}

/**
 * Where a sign-in lands: the screen the running version names as home for this device and the
 * signed-in person's roles (ADR-0210 decision 1, ADR-0213 decision 3), else `/` as before. Of two
 * of the person's roles with a home, the one listed first in the version wins. A home the till cannot open, or one that needs a table, lands on
 * `/` too.
 */
export function homeRoute(): string {
  const home = homeScreenId({ device_class: deviceClass(), role_ids: roleIds() });
  const route = home === undefined ? undefined : screenRoute(home);
  return route === undefined || route.includes(":id") ? "/" : route;
}

export function ScreenReactions() {
  const navigate = useNavigate();
  const location = useLocation();
  setScreenOpener((screenId, fired) => {
    const route = screenRoute(screenId);
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
