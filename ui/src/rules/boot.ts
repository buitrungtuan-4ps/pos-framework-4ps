// Starts the till's rules module once the store is loaded (`./till-rules.ts`): instantiated when the
// till is next idle, never on a tap, and loaded with the flows of the package it runs
// (`../state/flows.ts`), which compiles every condition once and again whenever they change. A tap
// then only asks.
//
// After the store's load, not at boot: building the evaluator is a few tens of milliseconds of main
// thread on a slow till, and done during sign-in or the store's first reads it would hold those reads
// back behind it. The module itself is fetched only once the flows need it (`../state/flows.ts`).

import { keepRulesLoaded } from "../state/flows";

/** How long the till waits for an idle moment before starting the module anyway. */
const START_WITHIN_MS = 2_000;

let booted = false;

/**
 * Instantiates the module when the till is next idle, and keeps the package's flows loaded in it.
 * Once: every later call does nothing.
 */
export function bootTillRules(): void {
  if (booted) {
    return;
  }
  booted = true;
  if (typeof requestIdleCallback === "function") {
    requestIdleCallback(keepRulesLoaded, { timeout: START_WITHIN_MS });
  } else {
    setTimeout(keepRulesLoaded, 0);
  }
}
