// Starts the till's rules module once the store is loaded (`./till-rules.ts`): instantiated when the
// till is next idle, never on a tap, and loaded with the flows of the package it draws, which
// compiles every condition once. A tap then only asks.
//
// After the store's load, not at boot: building the evaluator is a few tens of milliseconds of main
// thread on a slow till, and done during sign-in or the store's first reads it would hold those reads
// back behind it.

import DEFAULT_PACKAGE from "../package/default-package.json";
import type { Variant } from "../package/variant";
import { startTillRules, type Flow } from "./till-rules";

/** How long the till waits for an idle moment before starting the module anyway. */
const START_WITHIN_MS = 2_000;

/** The default package's flows: every variant of each, in the package's order. */
export function defaultFlows(): Flow[] {
  const flows = ((DEFAULT_PACKAGE as { flows?: Variant<Flow>[] }).flows ?? []) as Variant<Flow>[];
  return flows.map((variant) => variant.body);
}

let booted = false;

/**
 * Instantiates the module when the till is next idle, and loads the default package's flows. Once:
 * every later call does nothing.
 */
export function bootTillRules(): void {
  if (booted) {
    return;
  }
  booted = true;
  const start = () => {
    void startTillRules().then((rules) => rules?.load(defaultFlows()));
  };
  if (typeof requestIdleCallback === "function") {
    requestIdleCallback(start, { timeout: START_WITHIN_MS });
  } else {
    setTimeout(start, 0);
  }
}
