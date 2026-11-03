// A drop of surplus cash from the drawer to the safe (ADR-0186 decision 5): its own act, not a paid
// out, so reports keep it apart from expenses, with a reason from the store's own list for a drop
// where the cashier picks one. Out of `./store` by the size rule; the Shift screen says what was
// recorded and the shift's dropped total, and the drawer springs as it does for a paid out.

import { api } from "../api/client";
import type { CountBreakdown, DrawerOutcome } from "../api/schemas";
import { ownOverride } from "./drawer-access";
import { applyShift } from "./store";

/**
 * Records `amountMinor` dropped to the safe from `shiftId`'s drawer, answering what the drawer did.
 * The notes go with it where they make up the whole amount: a drop names no loose amount, so one
 * counted with some is recorded as its amount alone, which the edge reads as a drop typed as one.
 */
export async function recordSafeDrop(
  shiftId: string,
  amountMinor: number,
  breakdown?: CountBreakdown,
  reasonCodeId?: string,
): Promise<DrawerOutcome | undefined> {
  const notes = breakdown?.uncounted_amount ? undefined : breakdown?.denomination_counts;
  const drop = { amount_minor: amountMinor, denomination_counts: notes, reason_code_id: reasonCodeId };
  const response = await api.safeDrop(shiftId, { ...drop, ...ownOverride() });
  applyShift(response);
  return response.drawer_open;
}
