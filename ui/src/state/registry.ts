// Every piece of store data the till draws, declared once (ADR-0173 decision 1).
//
// Each entry names where it is read, how its body is applied, whether the till waits for it, and
// which configuration nodes it derives from; `./engine` runs them all the same way. The live
// entries (session, drawers, the open shift, live orders) join in the next change; until then they
// are read beside the engine, as they were.
//
// The nodes are the top-level nodes of the store's configuration each read is built from
// (`crates/pos-edge/src/config_client/apply.rs`). The price books also derive from settings,
// pricing and tender nodes, so they are read again for any changed node no other entry names. An
// entry read once too often costs one `304`; one read too rarely shows the old answer, so a node
// is only left off where the read cannot depend on it.

import { menuPath } from "../api/client";
import type {
  FloorResponse,
  LayoutResponse,
  LocaleResponse,
  MenuResponse,
  ReasonCodesResponse,
} from "../api/types";
import { configApplied, load, loadAll, register, type Entry } from "./engine";
import { loadSession } from "./session";
import {
  DINE_IN,
  TAKEAWAY,
  applyFloor,
  applyLayout,
  applyLocale,
  applyMenu,
  applyReasonCodes,
  loadDrawers,
  loadLiveOrders,
  loadShift,
  setWalkInBook,
  walkInChannel,
} from "./store";

const ENTRIES: readonly Entry[] = [
  {
    key: "floor",
    path: () => "/api/floor",
    apply: (body) => applyFloor(body as FloorResponse),
    required: true,
    nodes: ["floor", "stations"],
  },
  {
    key: "menu",
    path: () => menuPath(),
    apply: (body) => {
      applyMenu(body as MenuResponse);
      // Whether walk-ins are eaten in is in this book, so the dining room's is read once it lands.
      void load("menu_dine_in");
    },
    required: true,
    nodes: ["menu", "locale"],
    unclaimedNodes: true,
  },
  {
    key: "menu_takeaway",
    path: () => menuPath(TAKEAWAY),
    apply: (body) => setWalkInBook(TAKEAWAY, body as MenuResponse),
    required: false,
    nodes: ["menu", "locale"],
    unclaimedNodes: true,
  },
  {
    key: "menu_dine_in",
    path: () => menuPath(DINE_IN),
    apply: (body) => setWalkInBook(DINE_IN, body as MenuResponse),
    required: false,
    nodes: ["menu", "locale"],
    unclaimedNodes: true,
    // Only where the store's walk-ins are eaten in, or may be (ADR-0146).
    when: () => walkInChannel() !== "WALK_IN_CHANNEL_TAKEAWAY",
  },
  {
    key: "layout",
    path: () => "/api/layout",
    apply: (body) => applyLayout(body as LayoutResponse),
    required: false,
    nodes: ["layout"],
  },
  {
    key: "locale",
    path: () => "/api/locale",
    apply: (body) => applyLocale(body as LocaleResponse),
    required: true,
    nodes: ["locale"],
  },
  {
    key: "reason_codes",
    path: () => "/api/reason-codes",
    apply: (body) => applyReasonCodes(body as ReasonCodesResponse),
    required: false,
    // The reasons' names are in the store's language.
    nodes: ["reason_codes", "locale"],
  },
];

register(ENTRIES);

// What the till draws from the store's configuration, through the engine, and the session and the
// drawers beside it. The boot gate, sign-in and a `resync` read all of it; a `config_applied` reads
// what the frame says changed (ADR-0160 item 7, ADR-0173).
export async function loadConfiguration(changed?: readonly string[]): Promise<void> {
  await Promise.all([
    changed === undefined ? loadAll() : configApplied(changed),
    // One read of `GET /api/session` for the idle lock and the person's permissions both.
    loadSession(),
    // How many drawers the store keeps, which its configuration decides (ADR-0167).
    loadDrawers(),
  ]);
}

// Everything a device needs to sell: the configuration, what is open, and the shift.
//
// One function rather than five call sites, because there are two moments a device becomes able to
// sell and both have to load the same set: the boot gate, and signing in, which navigates
// client-side so `App`'s `onMount` does not run again.
export async function loadStore(): Promise<void> {
  await Promise.all([loadConfiguration(), loadLiveOrders(), loadShift()]);
}
