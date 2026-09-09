// Reading a caught API failure: the two questions every screen asks, in one place.
//
// # What repeated, and what did not
//
// Thirty-one screens each wrote `caught instanceof ApiError ? caught.message : String(caught)` —
// forty-three copies of one expression — while the five catalog screens imported a helper for it
// from `screens/catalog/shared.tsx`. That directory is why: a helper inside `catalog/` is a helper
// the rest of the console cannot see, so everybody else wrote it again.
//
// The consequence was not only duplication. `isStale` — ADR-0094's "somebody else saved this
// first", a `412` — lived in the same file, so **four screens that send an `etag` never handled the
// refusal it invites**: Activation, Campaigns, Inventory and ReasonCodes showed the raw server
// message for a conflict, where fourteen other screens explain it and reload. Nobody chose that;
// it followed from where the helper lived.
//
// What deliberately did *not* move here is the prose. Each stale-aware screen has its own message
// naming its own subject — "this store", "this station", "these tax rates", "this area or table" —
// and collapsing eight specific sentences into one generic "this record" would be worse writing for
// the sake of a smaller catalogue. The four screens fixed alongside this file therefore gained
// their own keys in that house style rather than a shared one.

import { ApiError } from "../api/client";

/**
 * The server's own message for a caught failure, or a stringified fallback.
 *
 * The fallback exists because a rejected promise is not always an `ApiError`: a dropped connection
 * or a parse failure arrives as something else, and a screen that assumed otherwise would render
 * `undefined` at the operator.
 */
export function apiMessage(caught: unknown): string {
  return caught instanceof ApiError ? caught.message : String(caught);
}

/**
 * Whether a caught failure is "somebody else saved this first" — ADR-0094's `412`.
 *
 * A screen that sends an `etag` invites this refusal and owes the reader two things: prose that says
 * what happened, and a reload, so they can see the change before deciding again. Retrying without
 * reloading would re-apply the overwrite the refusal exists to prevent.
 */
export function isStale(caught: unknown): boolean {
  return caught instanceof ApiError && caught.isStale;
}

/**
 * Runs a conditional write so that a stale refusal reloads the screen and comes back in the
 * screen's own words, and every other failure passes through untouched.
 *
 * Both halves matter and each was missing somewhere. The **reload** is ADR-0094's own reasoning:
 * retrying a `412` without reloading would re-apply the overwrite the refusal exists to prevent, so
 * the operator has to see what changed before deciding again. The **rewording** is why this returns
 * a fresh `ApiError` rather than re-throwing: the server's message for a `412` names a version, and
 * the twelve `*.stale` catalogue entries are the sentences written for an operator ("Someone else
 * changed this store while you were editing…"). Re-throwing loses them — which is exactly what
 * happened when the first screen was moved onto `EntityCrud`, because the reload was the visible
 * half and the prose was not.
 *
 * The replacement stays a stale `ApiError` — same status, same canonical token — so anything
 * downstream that asks `isStale` still gets a yes. Composes inside `EntityCrud.run`, which keeps
 * the form open carrying whatever message this ends up with.
 */
export async function withStaleReload<T>(
  write: () => Promise<T>,
  reload: () => Promise<unknown>,
  prose: string,
): Promise<T> {
  try {
    return await write();
  } catch (caught) {
    if (!isStale(caught)) {
      throw caught;
    }
    await reload();
    throw new ApiError(412, prose, "FAILED_PRECONDITION");
  }
}
