// Every piece of store data the till draws, declared once (ADR-0173 decision 1).
//
// Each entry names its kind, where it is read, how its body is applied, whether the till waits for
// it, and which configuration nodes it derives from; `./engine` runs them all the same way.
//
// **Live entries are never kept on the device** (ADR-0173 decision 3): the session and its
// permissions, the drawers (which name the person a drawer is assigned to), the shift and the open
// orders are read from the edge every time. Only an entry of kind `configuration` may be.
//
// The nodes are the top-level nodes of the store's configuration each read is built from
// (`crates/pos-edge/src/config_client/apply.rs`). The price books also derive from settings,
// pricing and tender nodes, so they are read again for any changed node no other entry names. An
// entry read once too often costs one `304`; one read too rarely shows the old answer, so a node
// is only left off where the read cannot depend on it.

import { menuPath, type SessionState } from "../api/client";
import type {
  DrawersResponse,
  FloorResponse,
  LayoutResponse,
  LocaleResponse,
  MenuResponse,
  ReasonCodesResponse,
  ShiftResponse,
} from "../api/types";
import { configApplied, load, loadAll, register, type Entry } from "./engine";
import { takeSession } from "./session";
import {
  DINE_IN,
  TAKEAWAY,
  applyDrawers,
  applyFloor,
  applyLayout,
  applyLiveOrders,
  applyLocale,
  applyMenu,
  applyReasonCodes,
  applyShift,
  readLiveOrders,
  setWalkInBook,
  type LiveOrdersRead,
  walkInChannel,
} from "./store";

const ENTRIES: readonly Entry[] = [
  {
    key: "floor",
    kind: "configuration",
    path: () => "/api/floor",
    apply: (body) => applyFloor(body as FloorResponse),
    required: true,
    nodes: ["floor", "stations"],
  },
  {
    key: "menu",
    kind: "configuration",
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
    kind: "configuration",
    path: () => menuPath(TAKEAWAY),
    apply: (body) => setWalkInBook(TAKEAWAY, body as MenuResponse),
    required: false,
    nodes: ["menu", "locale"],
    unclaimedNodes: true,
  },
  {
    key: "menu_dine_in",
    kind: "configuration",
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
    kind: "configuration",
    path: () => "/api/layout",
    apply: (body) => applyLayout(body as LayoutResponse),
    required: false,
    nodes: ["layout"],
  },
  {
    key: "locale",
    kind: "configuration",
    path: () => "/api/locale",
    apply: (body) => applyLocale(body as LocaleResponse),
    required: true,
    nodes: ["locale"],
  },
  {
    key: "reason_codes",
    kind: "configuration",
    path: () => "/api/reason-codes",
    apply: (body) => applyReasonCodes(body as ReasonCodesResponse),
    required: false,
    // The reasons' names are in the store's language.
    nodes: ["reason_codes", "locale"],
  },
  {
    key: "session",
    kind: "live",
    path: () => "/api/session",
    apply: (body) => takeSession(body as SessionState),
    // Who is signed in and what they may do; the permissions are the published `permissions`
    // node, and the idle lock and opening float are settings (ADR-0158, ADR-0160).
    required: true,
    nodes: ["permissions"],
    unclaimedNodes: true,
  },
  {
    key: "drawers",
    kind: "live",
    path: () => "/api/shifts",
    apply: (body) => applyDrawers(body as DrawersResponse),
    // How many drawers the store keeps and how they are shared, which its settings decide
    // (ADR-0167).
    required: false,
    nodes: ["shift", "devices"],
    unclaimedNodes: true,
  },
  {
    key: "shift",
    kind: "live",
    path: () => "/api/shifts/current",
    apply: (body) => applyShift(body as ShiftResponse | null),
    required: false,
    // Whether the count is blind is a setting, so the expectation it carries can change.
    nodes: ["shift"],
  },
  {
    key: "live_orders",
    kind: "live",
    path: () => "/api/orders/live",
    read: readLiveOrders,
    apply: (body) => applyLiveOrders(body as LiveOrdersRead),
    required: true,
    nodes: [],
  },
];

register(ENTRIES);

// Reads again what derives from the store's configuration: a `config_applied`, which names the
// nodes it changed where the edge can say (ADR-0160 item 7, ADR-0173).
export async function loadConfiguration(changed?: readonly string[]): Promise<void> {
  await configApplied(changed);
}

// Everything a device needs to sell: the configuration, the session, the drawers, the shift and
// what is open, but those in `except`, which the caller already holds.
//
// Two moments a device becomes able to sell have to load the same set: the boot gate, and signing
// in, which navigates client-side so `App`'s `onMount` does not run again.
export async function loadStore(except: readonly string[] = []): Promise<void> {
  await loadAll(except);
}
