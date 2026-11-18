// Whether the store pays first (`pay_first_enabled`, ADR-0222 decision 6), from the menu read. On
// such a store settling an order with no table sends its unsent lines to the kitchen in the settle's
// own transaction, so the counter does not ask to send them before payment (`./unsent.ts`).

import { createSignal } from "solid-js";

import type { MenuResponse } from "../api/schemas";

const [payFirst, setPayFirst] = createSignal(false);

/** Applies the menu read's flag; an edge older than it pays as before, and the till asks. */
export function applyPayFirst(response: MenuResponse): void {
  setPayFirst(response.pay_first_enabled === true);
}

/** Whether settling an order with no table sends its unsent lines itself. */
export function paysFirst(): boolean {
  return payFirst();
}
