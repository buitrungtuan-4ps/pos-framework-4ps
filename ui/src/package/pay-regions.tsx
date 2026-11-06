// The pay screen's parts as catalogued components (ADR-0190, ADR-0203 decision 1).
//
// The pay screen keeps its state and its taps in `screens/Pay.tsx`, where the step gate resolves
// them, and hands each region to these components through `PayContext`. A region drawn on a screen
// that is not the pay screen draws nothing.

import { createContext, useContext, type JSX } from "solid-js";

export interface PayRegions {
  readonly back: () => JSX.Element;
  readonly flow: () => JSX.Element;
}

export const PayContext = createContext<PayRegions>();

// `pay_back`: the way back to the order.
export function PayBack(): JSX.Element {
  return useContext(PayContext)?.back() ?? null;
}

// `pay_flow`: the amount due and the bill's arithmetic, the tenders, splitting and reductions, the
// settle and what it answered, or what ended the bill.
export function PayFlow(): JSX.Element {
  return useContext(PayContext)?.flow() ?? null;
}
