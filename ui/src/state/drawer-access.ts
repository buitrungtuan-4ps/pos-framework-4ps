// Whose a drawer is, where the store assigns drawers
// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 9), and the
// override approver an act on somebody else's drawer takes.
//
// `GET /api/shifts` says `DRAWER_ACCESS_ASSIGNED`, and each shift names its person by
// `assigned_employee_id` alone: the store server keeps no staff names, so the till never shows one.
// It tells the signed-in person's own drawer from somebody else's by comparing ids, and says only
// which of the two it is.
//
// An act on somebody else's drawer takes `cash.drawer.override_assignment`, which a person may hold
// only with approval. The approver typed for it is held here, for the screen showing the drawer,
// and sent beside the act by `state/store.ts`; it is forgotten when that screen goes, so a PIN does
// not outlive the panel it was typed on.

import { createSignal } from "solid-js";

import type { ApproverRequest } from "../api/types";
import { asksApprover } from "./permissions";
import { employeeId } from "./session";
import { state } from "./store";

const [code, setCode] = createSignal("");
const [pin, setPin] = createSignal("");

export { code as overrideCode, pin as overridePin, setCode as setOverrideCode };
export { setPin as setOverridePin };

// Whether each of the store's drawers is one person's for its session.
export function drawersAssigned(): boolean {
  return state.drawers?.drawer_access === "DRAWER_ACCESS_ASSIGNED";
}

// Whether a drawer assigned to `assignee` is somebody other than the signed-in person's.
export function somebodyElses(assignee: string | undefined): boolean {
  return drawersAssigned() && assignee !== undefined && assignee !== employeeId();
}

// Whether acting on that drawer asks for an approver first: the person does not hold the override
// directly, or the store does not enforce each person's own set (`asksApprover`).
export function overrideAsks(assignee: string | undefined): boolean {
  return somebodyElses(assignee) && asksApprover("cash.drawer.override_assignment");
}

// The approver an act on a drawer assigned to `assignee` carries: the one typed here where it asks
// for one, and nothing otherwise.
export function overrideApproval(assignee: string | undefined): Partial<ApproverRequest> {
  return overrideAsks(assignee) && code().trim() !== ""
    ? { approver_code: code().trim(), approver_pin: pin() }
    : {};
}

// The approver an act on this till's own drawer carries, where it is another person's.
export function ownOverride(): Partial<ApproverRequest> {
  return overrideApproval(state.shift?.assignee);
}

// Forgets the approver typed.
export function forgetOverride(): void {
  setCode("");
  setPin("");
}
