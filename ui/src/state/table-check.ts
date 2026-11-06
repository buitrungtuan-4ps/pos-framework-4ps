// What a table owes right now, asked of the edge — the same `billing::assemble` the settle runs, so
// the guest is quoted the figure the bill will charge (roadmap-v3 E5).
//
// Until the order screen showed it, the only way to answer "how much so far?" was to press Take
// payment, and that **opens a bill**: a guest's question changed the order's state. The fingerprint
// below is what the edge would price differently — a line appearing, disappearing, or changing
// state — so the figure refreshes on every act without polling. A line still sending is left out:
// the edge has not priced it, and its confirmation re-reads.

import { createMemo, createResource, untrack, type Accessor } from "solid-js";

import type { CheckResponse } from "../api/schemas";
import { afterPaint } from "../lib/after-paint";
import { linesForTable, loadCheck } from "./store";

/** A table's check, as the order screen shows it. */
export interface TableCheck {
  /** The figure for the table shown now, or `undefined` until the edge has priced it. */
  readonly check: Accessor<CheckResponse | undefined>;
  /** Reads it again, after an act that changes the figure without changing a line. */
  readonly refetch: () => void;
}

/**
 * The check of the table `key` names, read while `reads` holds.
 *
 * - A read that fails is "no figure yet" rather than an error the resource keeps: a resource that
 *   has thrown stays errored, so one failed read used to leave the total at "—" through every read
 *   that followed. The next change reads it again.
 * - The read leaves after the frame that shows the act that changed the figure (`afterPaint`), so
 *   that frame carries the act alone.
 * - The figure is kept with the table it priced. A kept order screen that has just changed tables
 *   shows the dash until the new table's figure lands, never the last table's, and a hidden one
 *   reads nothing (`../screens/kept.tsx`).
 */
export function createTableCheck(
  key: Accessor<string>,
  reads: Accessor<boolean>,
): TableCheck {
  const priceable = createMemo(() =>
    linesForTable(key())
      .filter((line) => line.pending !== true)
      .map(
        (line) =>
          `${line.orderLineId}:${line.state}:${line.quantityMilli}`,
      )
      .join(","),
  );
  const [priced, { refetch }] = createResource(
    () => reads() && priceable(),
    async () => {
      const pricedKey = key();
      await afterPaint();
      // A screen hidden, or on another table, by the time the frame is drawn reads nothing: it is
      // read again when it shows, and a read of a table it has left would be one too many.
      if (!untrack(reads) || untrack(key) !== pricedKey) {
        return undefined;
      }
      return loadCheck(pricedKey).then(
        (totals) => ({ key: pricedKey, totals }),
        () => undefined,
      );
    },
  );
  return {
    check: () => {
      const figure = priced();
      return figure !== undefined && figure.key === key()
        ? figure.totals
        : undefined;
    },
    refetch: () => void refetch(),
  };
}
