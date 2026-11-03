// A drop of surplus cash from the drawer to the safe (ADR-0186 decision 5): its own act, not a paid
// out, so reports keep it apart from expenses. Out of `./store` by the size rule; the Shift screen
// says what was recorded, and the drawer springs as it does for a paid out.

import { api } from "../api/client";
import type { DrawerOutcome } from "../api/schemas";
import { ownOverride } from "./drawer-access";

/** Records `amountMinor` dropped to the safe from `shiftId`'s drawer, answering what the drawer did. */
export async function recordSafeDrop(shiftId: string, amountMinor: number): Promise<DrawerOutcome | undefined> {
  const response = await api.safeDrop(shiftId, { amount_minor: amountMinor, ...ownOverride() });
  return response.drawer_open;
}
