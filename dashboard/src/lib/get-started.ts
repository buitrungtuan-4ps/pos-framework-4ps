// The activation path, as a checklist: what an admin must do, in order, before a shop can trade.
//
// # Why this exists
//
// Signing into a fresh console lands on a store hub with no store, which says "choose it in the top
// bar" — advice that cannot be followed, because there is nothing to choose. The work that has to
// happen is a chain: a tenant, then a store under it, then a key that store's box can present, then
// a published configuration, then the box installed and a till admitted, and only then does the
// shop report. Every link of that chain has a screen in this console, and nothing anywhere says
// what the chain *is*. An operator learns it from a runbook or from somebody who already knows.
//
// # Read-only, and assembled from reads that already exist
//
// No new route and no new state: each step's answer is a list this console already fetches
// somewhere else, and "done" is that list being non-empty. That is the same discipline ADR-0099 set
// for the hub, and it is what makes the checklist safe to render on the landing screen — it can go
// wrong by being *wrong*, never by breaking something.
//
// # Five statuses, because "not done" and "could not look" are different answers
//
// A step whose read is still in flight is `checking`; one whose read was refused — a role without
// the permission, a cloud that is down — is `unknown`, and says so rather than reporting a red
// cross for work that may well be finished. A step gated behind an earlier one is `waiting`: with
// no tenant chosen, whether a brand exists is not false, it is unasked.

import type { MessageKey } from "../i18n";
import type { Panel } from "./panel";
import type { ScreenId } from "../state/screens";

/** Every step of the chain, in the order the work has to happen. */
export type StepId = "tenant" | "brand" | "store" | "apiKey" | "config" | "device" | "reporting";

/**
 * Where a step stands.
 *
 * `todo` is a claim — we asked and the answer was no. `unknown` and `waiting` are refusals to
 * claim: the read failed, or its prerequisite is not met so nothing was read. Collapsing those
 * three into one "not done" is how a checklist starts lying to an operator who lacks a permission.
 */
export type StepStatus = "checking" | "done" | "todo" | "waiting" | "unknown";

/** One step: what it is called, what it means, where it is done, and whether it is required. */
export type Step = {
  readonly id: StepId;
  readonly title: MessageKey;
  readonly hint: MessageKey;
  /**
   * The screen that does this step and the label of the link to it, when one screen does it.
   *
   * One field rather than two so "has a link" and "has a label for it" cannot disagree. Each label
   * names its destination: seven links all reading "Open" would leave a screen reader announcing
   * the same name seven times, which is a list of links to nowhere in particular.
   */
  readonly go?: { readonly screen: ScreenId; readonly label: MessageKey };
  /** When true, a shop can trade without it — shown, but not counted against completion. */
  readonly optional?: boolean;
};

/**
 * The chain, top-down, mandatory before optional.
 *
 * `tenant` has no link on purpose: a tenant is created in the context picker in the top bar, which
 * is not a screen and has no URL. The step's hint says where instead of pointing at a route that
 * does not exist.
 *
 * `brand` is optional because `POST /admin/stores` takes `brand_id` and does not require it — a
 * store can be created and can trade without one. It stays on the list because a fork that skips
 * brands and later wants them has to restate every store's identity; but marking it required would
 * make the checklist demand work the server does not.
 */
export const STEPS: readonly Step[] = [
  { id: "tenant", title: "getStarted.tenant.title", hint: "getStarted.tenant.hint" },
  {
    id: "brand",
    title: "getStarted.brand.title",
    hint: "getStarted.brand.hint",
    go: { screen: "stores", label: "getStarted.brand.action" },
    optional: true,
  },
  {
    id: "store",
    title: "getStarted.store.title",
    hint: "getStarted.store.hint",
    go: { screen: "newStore", label: "getStarted.store.action" },
  },
  {
    id: "apiKey",
    title: "getStarted.apiKey.title",
    hint: "getStarted.apiKey.hint",
    go: { screen: "apiKeys", label: "getStarted.apiKey.action" },
  },
  {
    id: "config",
    title: "getStarted.config.title",
    hint: "getStarted.config.hint",
    go: { screen: "config", label: "getStarted.config.action" },
  },
  {
    id: "device",
    title: "getStarted.device.title",
    hint: "getStarted.device.hint",
    go: { screen: "activation", label: "getStarted.device.action" },
  },
  {
    id: "reporting",
    title: "getStarted.reporting.title",
    hint: "getStarted.reporting.hint",
    go: { screen: "fleet", label: "getStarted.reporting.action" },
  },
];

/**
 * What the console has managed to read.
 *
 * Counts rather than lists: the checklist only ever asks "is there one", and a count cannot carry a
 * name, a ULID or a key id by accident. `keys` is already narrowed to keys this store can actually
 * present (S1 binds a store's credential to the store), and `reporting` is whether the store has
 * ever checked in — not whether it is online now, which a closed shop is not.
 */
export type Progress = {
  readonly tenantChosen: boolean;
  readonly storeChosen: boolean;
  readonly tenants: Panel<number>;
  readonly brands: Panel<number>;
  readonly stores: Panel<number>;
  readonly keys: Panel<number>;
  readonly versions: Panel<number>;
  readonly devices: Panel<number>;
  readonly reporting: Panel<boolean>;
};

function settled<T>(panel: Panel<T>, done: (value: T) => boolean): StepStatus {
  switch (panel.state) {
    case "loading":
      return "checking";
    case "failed":
      return "unknown";
    default:
      return done(panel.value) ? "done" : "todo";
  }
}

const some = (count: number) => count > 0;

/** Where each step stands, given what has been read. */
export function statuses(progress: Progress): Record<StepId, StepStatus> {
  const tenantGated = <T,>(panel: Panel<T>, done: (value: T) => boolean): StepStatus =>
    progress.tenantChosen ? settled(panel, done) : "waiting";
  const storeGated = <T,>(panel: Panel<T>, done: (value: T) => boolean): StepStatus =>
    progress.storeChosen ? settled(panel, done) : "waiting";
  return {
    tenant: settled(progress.tenants, some),
    brand: tenantGated(progress.brands, some),
    store: tenantGated(progress.stores, some),
    apiKey: storeGated(progress.keys, some),
    config: storeGated(progress.versions, some),
    device: storeGated(progress.devices, some),
    reporting: storeGated(progress.reporting, (ever) => ever),
  };
}

/** How many required steps are done, out of how many there are — the checklist's one-line summary. */
export function requiredProgress(by: Record<StepId, StepStatus>): {
  readonly done: number;
  readonly total: number;
} {
  const required = STEPS.filter((step) => step.optional !== true);
  return {
    done: required.filter((step) => by[step.id] === "done").length,
    total: required.length,
  };
}

/**
 * Whether the chain is finished and the checklist should stand down.
 *
 * Only `done` counts. A `checking` step is not finished yet and an `unknown` one was never
 * established, so a console that cannot read its own state keeps the checklist up rather than
 * quietly declaring victory.
 */
export function setupComplete(by: Record<StepId, StepStatus>): boolean {
  const { done, total } = requiredProgress(by);
  return done === total;
}
