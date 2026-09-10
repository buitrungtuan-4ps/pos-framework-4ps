// Whether a replacement machine has taken this store, as the store server reports it on every
// answer (ADR-0123, closing ADR-0049's edge half).
//
// # Why a response header, again
//
// Same reason `edgeVersion` uses one, and it applies harder here: a supersession happens *during*
// service. An operator activates a new box at four in the afternoon, and from that moment this one
// will not seat another table. A value read once at pairing time — or once at sign-in — is a value
// that was true once, and the till would go on offering a Seat button that answers `403`.
//
// Every `/api/*` answer carries `pos-lease-standing`, so the app learns the box's standing on the
// call it just made, including the call that just failed. No poll, no second endpoint, and nothing
// to keep in sync with the gate: the header is stamped from the very cell the refusal reads.
//
// # What it costs, and what it does not
//
// A banner. It does not disable a button and it does not navigate anywhere — the operator may still
// need every screen on this box to finish the tables it holds, and a till that hides its own
// controls is harder to drain than one that explains itself. The refusal itself lives on the store
// server, where it cannot be worked around by a stale tab.

import { createSignal } from "solid-js";

// The header the edge stamps on every `/api/*` response.
const HEADER = "pos-lease-standing";

// The three answers `pos_core::lease::LeaseStanding` can give, plus `null` for "no answer yet".
export type LeaseStanding = "active" | "superseded" | "invalid";

const [leaseStanding, setLeaseStanding] = createSignal<LeaseStanding | null>(null);
export { leaseStanding };

// Records the standing that answered, if the response named one it recognises.
//
// Deliberately strict about the vocabulary: a value this does not know is left as the previous
// answer rather than shown as one. A newer store server that grows a fourth standing must not make
// an older app draw a banner it has no words for — and must not silently clear a `superseded` the
// app was right to be showing.
export function observeLeaseStanding(response: Response): void {
  const reported = response.headers.get(HEADER);
  if (reported === "active" || reported === "superseded" || reported === "invalid") {
    setLeaseStanding(reported);
  }
}

// Whether this box has been replaced and will refuse to open anything new.
//
// False until a response has been seen — a box that has not answered yet has not said it is
// superseded, and a banner on first paint would be a guess.
export function edgeIsSuperseded(): boolean {
  return leaseStanding() === "superseded";
}

// Whether this box claims a lease generation ahead of the one the cloud published.
//
// It keeps trading (ADR-0123 decision 2): under take-once the likeliest cause is a config rollback,
// and refusing to sell would fire on every box in the store at once. But it is a state somebody
// should look at, and the till is where a person is.
export function edgeLeaseIsAhead(): boolean {
  return leaseStanding() === "invalid";
}
