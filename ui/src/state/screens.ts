// The till's screens, in one table: the single list the router is built from (ADR-0170 decision 5,
// as `dashboard/src/state/screens.ts` is for the console).
//
// # Why one table
//
// The till's routes used to be a hand-written list of `<Route>`s in `App.tsx`, with nothing to say
// which screens exist apart from that list. Now a `ScreenId` is a union of this table's keys, and
// `App.tsx` maps every id to its component in a `Record<ScreenId, Component>`: a screen added here
// without a component, or a component left for a screen removed, does not compile, so the router
// cannot fall behind the table.
//
// The claim page (`/claim`, ADR-0148) is not here on purpose. A box being claimed is not a store
// server yet, so `App.tsx` gives that page alone, before the router and the shell exist.

import type { MessageKey } from "../i18n";

/** One screen: where it lives and what it is called. */
export type Screen = {
  /** The route path, with `:id` for a parameter. */
  readonly path: string;
  /** The i18n key of its title, where the screen has a fixed one. An order's title names the order. */
  readonly key?: MessageKey;
};

export const SCREENS = {
  floor: { path: "/", key: "floor.title" },
  tableOrder: { path: "/table/:id" },
  tablePay: { path: "/table/:id/pay" },
  kds: { path: "/kds", key: "kds.title" },
  expo: { path: "/expo", key: "expo.title" },
  counter: { path: "/counter", key: "counter.title" },
  counterOrder: { path: "/order/:id" },
  // The staff-confirmation queue (ADR-0116): the screen the QR hold had no way to reach.
  guests: { path: "/guests", key: "confirm.title" },
  today: { path: "/today", key: "today.title" },
  shift: { path: "/shift", key: "shift.title" },
  pair: { path: "/pair", key: "pair.title" },
  devices: { path: "/devices", key: "devices.title" },
  setup: { path: "/setup", key: "setup.title" },
  signIn: { path: "/signin", key: "signin.title" },
} as const satisfies Record<string, Screen>;

/** Every screen's id. A `ScreenId` that is not in [`SCREENS`] does not compile. */
export type ScreenId = keyof typeof SCREENS;

/** Every screen's id, in the table's order. */
export const SCREEN_IDS = Object.keys(SCREENS) as ScreenId[];
