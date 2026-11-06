// Starts the till's rules module once the first screen is drawn (`./till-rules.ts`): instantiated
// when the till is idle, never on a tap, and loaded with the flows of the package it draws, which
// compiles every condition once. A tap then only asks.

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

/** Instantiates the module when the till is next idle, and loads the default package's flows. */
export function bootTillRules(): void {
  const start = () => {
    void startTillRules().then((rules) => rules?.load(defaultFlows()));
  };
  if (typeof requestIdleCallback === "function") {
    requestIdleCallback(start, { timeout: START_WITHIN_MS });
  } else {
    setTimeout(start, 0);
  }
}
