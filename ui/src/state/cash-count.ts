// A drawer counted note by note (ADR-0186 decisions 1 and 3): the store's counting list, and what a
// count field on the Shift screen records. Out of `./store` by the size rule.
//
// The counting list is not the quick-cash list. `cash_denominations` is what a guest hands over, for
// the Pay screen's keys; `cash_count_denominations` is every note and coin in the drawer. An empty
// list, or an edge too old to send one, means no counter: the field stays one typed amount.

import { createSignal } from "solid-js";

import type { CountBreakdown, DenominationCount } from "../api/schemas";
import { money } from "../lib/money";
import { parseAmount, storeCurrency } from "./store";

/** What a counter holds: its total, the rows counted, and the amount the list does not name, in minor units. */
export interface CounterValue {
  total: number;
  rows: readonly DenominationCount[];
  uncounted: number;
}

/** A counter with nothing counted. */
export const EMPTY_COUNT: CounterValue = { total: 0, rows: [], uncounted: 0 };

const [denominations, setDenominations] = createSignal<readonly number[]>([]);

/** The notes and coins this store counts a drawer by, ascending, in minor units; empty for no counter. */
export const countDenominations = denominations;

/** Applies the store's counting list from `GET /api/locale`; absent from an older edge, which is none. */
export function applyCountDenominations(list: readonly number[] | undefined): void {
  setDenominations(list ?? []);
}

/**
 * What a count field records: the counter's total and its rows where the store counts by note, else
 * the typed figure, or `null` while that is not a figure. The total is computed, so it always adds up.
 */
export function fieldAmount(
  text: string,
  count: CounterValue,
): { minor: number; breakdown?: CountBreakdown } | null {
  if (countDenominations().length === 0) {
    const minor = parseAmount(text);
    return minor === null ? null : { minor };
  }
  const uncounted = count.uncounted > 0 ? money(storeCurrency(), count.uncounted) : undefined;
  return {
    minor: count.total,
    breakdown: { denomination_counts: [...count.rows], uncounted_amount: uncounted },
  };
}
