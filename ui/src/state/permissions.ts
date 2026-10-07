// What the signed-in person may do at this till, as `GET /api/session` reports it
// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md) decision 6),
// and the one place a screen asks.
//
// The edge stays the authority: it refuses what a person may not do whatever the till shows. This
// keeps the till from offering an act the edge would refuse, and from sending one that needs another
// person's approval without asking for the approver first.
//
// Where the store does not enforce each person's own set — `permissions_enforced` false or absent,
// which is every store until an owner turns it on and every edge too old to say — the edge decides
// with a store-wide set that grants everything, so `can` is true for every act, `needsApprover` is
// false, and nothing is hidden. That is also the state before the first read lands: a till must not
// hide a store's controls on the strength of a read that has not arrived, for `tablesEnabled`'s
// reason in `store.ts`.

import { createSignal } from "solid-js";

import type { SessionState } from "../api/client";

/**
 * A catalogue permission a till control needs, by its id. Each is named by a route in
 * `ROUTE_PERMISSIONS` (`crates/pos-edge/src/http/route_permissions.rs`), which is how a control is
 * mapped to what it needs; a union rather than `string`, so a mistyped id is a type error rather
 * than a control hidden from everybody.
 */
export type PermissionId =
  | "admin.device.manage"
  | "billing.bill.open"
  | "billing.bill.split"
  | "billing.bill.void"
  | "billing.discount.apply"
  | "billing.discount.override_ceiling"
  | "billing.fee.waive"
  | "billing.payment.take"
  | "billing.receipt.reprint"
  | "cash.drawer.open_no_sale"
  | "cash.movement.record"
  | "cash.shift.close"
  | "cash.shift.manage_other_till"
  | "cash.shift.open"
  | "reports.takings.view"
  | "sales.item.mark_unavailable"
  | "sales.line.add"
  | "sales.line.fire"
  | "sales.line.void_fired"
  | "sales.order.confirm_qr"
  | "sales.order.transfer"
  | "sales.table.manage"
  | "sales.ticket.bump";

// The acts that take another person's approval today: voiding a fired line or a bill, a discount
// above the ceiling, waiving a fee, opening the drawer without a sale, and starting, counting or
// closing another till's drawer (`docs/pos-spec.md` §9, ADR-0167 decision 3). Any other permission
// the edge accepts only when held directly, so holding one of those with approval lets the person
// do nothing at this till.
const TAKES_APPROVER: ReadonlySet<PermissionId> = new Set<PermissionId>([
  "sales.line.void_fired",
  "billing.bill.void",
  "billing.discount.override_ceiling",
  "billing.fee.waive",
  "cash.drawer.open_no_sale",
  "cash.shift.manage_other_till",
]);

interface Held {
  enforced: boolean;
  direct: ReadonlySet<string>;
  withApproval: ReadonlySet<string>;
  // What the person's own roles grant directly, whatever the switch says: what `holdsOwn` reads.
  own: ReadonlySet<string>;
}

const NOTHING: ReadonlySet<string> = new Set();

const [held, setHeld] = createSignal<Held>({
  enforced: false,
  direct: NOTHING,
  withApproval: NOTHING,
  own: NOTHING,
});

// Takes what a session read says. The two lists `can` reads are kept only where the store enforces
// them: elsewhere they are what the person's roles say, not what the edge decides with. What the
// roles grant directly is kept whatever the switch, for `holdsOwn`: for those acts it is what the
// edge decides with in every store.
export function adoptSession(session: SessionState): void {
  const enforced = session.permissions_enforced === true;
  setHeld({
    enforced,
    direct: enforced ? new Set(session.permissions ?? []) : NOTHING,
    withApproval: enforced ? new Set(session.permissions_with_approval ?? []) : NOTHING,
    own: new Set(session.permissions ?? []),
  });
}

// The session is read again at sign-in, whenever the store's configuration is read again — a
// published `permissions` node is what changes the two lists — and when a PIN opens the idle lock,
// by `loadSession` in `./session`, the one reader, which hands each read here. A failed read keeps
// what is held: a till that hid or showed everything on a blip would be worse than one briefly out
// of date.

// Nobody is signed in any more, so nobody holds anything: what the edge reports with nobody signed
// in. The switch stays as the store last said, and a store that does not enforce still hides nothing.
export function forgetPermissions(): void {
  setHeld((current) => ({
    enforced: current.enforced,
    direct: NOTHING,
    withApproval: NOTHING,
    own: NOTHING,
  }));
}

// Whether the store decides with each person's own set.
export function permissionsEnforced(): boolean {
  return held().enforced;
}

// Whether the signed-in person may do this act: they hold it directly, or with approval where the act
// takes an approver. Always true where the store does not enforce.
export function can(id: PermissionId): boolean {
  const { enforced, direct, withApproval } = held();
  return !enforced || direct.has(id) || (withApproval.has(id) && TAKES_APPROVER.has(id));
}

// Whether the person's own roles grant this directly, whatever the store's switch says: for what the
// edge decides from the person's own role in every store, and the day's takings is one
// (ADR-0160 decision 2). Takings are confidential and nobody saw them at a till before, so a store
// that does not enforce yet has nothing here to keep working, and the till shows them only to whom
// the edge will serve them. False before the first session read lands.
export function holdsOwn(id: PermissionId): boolean {
  return held().own.has(id);
}

// Whether they may do at least one of these: a screen with several acts is a destination for anybody
// who may do one of them.
export function canAny(ids: readonly PermissionId[]): boolean {
  return ids.some(can);
}

// Whether the act needs another person's code and PIN: the person holds it only with approval. False
// for everything where the store does not enforce.
export function needsApprover(id: PermissionId): boolean {
  const { enforced, direct, withApproval } = held();
  return enforced && !direct.has(id) && withApproval.has(id) && TAKES_APPROVER.has(id);
}

// Whether the till asks for an approver before sending one of the acts that take one. Where the store
// enforces, exactly when `needsApprover` says so: a person who holds the act directly does it alone,
// and one who holds it neither way is refused by the edge whoever approves. Elsewhere always, as the
// till always has: the store-wide set holds nothing directly, so the edge asks for a holder's PIN.
export function asksApprover(id: PermissionId): boolean {
  return held().enforced ? needsApprover(id) : true;
}
