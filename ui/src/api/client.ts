// The typed HTTP client for the edge's domain routes. Every call is one `fetch` to `base() + path`,
// where the base is empty for a till served by the box it talks to — so an in-store device sends the
// identical root-relative request it always has, and a native shell or a hosted placement sends the
// same path against the origin it paired with (ADR-0111). A refused command comes back as a non-2xx
// with a plain-text reason (the edge maps a domain refusal to 409); this surfaces it as an
// `ApiError` the screens can show without guessing.

import { clearDeviceToken, deviceToken, edgeBase, rememberPairing } from "./credentials";
import { observeEdgeVersion } from "./edgeVersion";
import { type LeaseStanding, observeLeaseStanding } from "./leaseStanding";
import type {
  ActivateAccepted,
  ActivationStanding,
  BindResponse,
  ClaimStatus,
  BillResponse,
  BumpRequest,
  BumpResponse,
  CheckResponse,
  CashMovementRequest,
  CountShiftRequest,
  BillCheckResponse,
  CounterOrder,
  DiscountRequest,
  DiscountResponse,
  DrawersResponse,
  FireRequest,
  QuantityRequest,
  FloorResponse,
  LineRequest,
  LineResponse,
  LayoutResponse,
  LiveOrder,
  LocaleResponse,
  MergeRequest,
  MenuResponse,
  MintedCode,
  OpenedOrder,
  OpenDrawerRequest,
  OpenDrawerResponse,
  OpenShiftRequest,
  OrderLineRequest,
  PairAccepted,
  PairingState,
  IntegrationEntry,
  PrinterEntry,
  PrintResponse,
  ReasonCodesResponse,
  ReprintResponse,
  SettleRequest,
  SettledBill,
  TakingsResponse,
  ShiftResponse,
  SplitRequest,
  SplitResponse,
  SyncResponse,
  TableResponse,
  TerminalsResponse,
  TestPrintResponse,
  TransferResponse,
  VoidBillResponse,
  VoidRequest,
  WaitingResponse,
  WaiveFeeRequest,
} from "./types";

export class ApiError extends Error {
  readonly status: number;
  // The stable token the edge names a refusal by (`pos-error-reason`, ADR-0137), or null for an edge
  // too old to send one. The message is the edge's English sentence; this is what a screen
  // translates.
  readonly reason: string | null;

  constructor(status: number, message: string, reason: string | null = null) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.reason = reason;
  }

  // A refused command (illegal move, missing permission, underpaid bill) — the caller's fault, worth
  // showing the operator, not a server failure to retry.
  get isConflict(): boolean {
    return this.status === 409;
  }

  // The device is not (or no longer) paired: the edge refuses a domain call without a valid device
  // token (ADR-0084). A screen sees this and sends the operator to pair.
  get isUnauthorized(): boolean {
    return this.status === 401;
  }

  // The device is paired but nobody is signed in: the edge refuses a command until an employee signs
  // in (S0b, ADR-0084). Distinct from `isUnauthorized` so the app shows the sign-in screen rather than
  // sending the operator back to pair.
  get needsSignIn(): boolean {
    return this.status === 403;
  }
}

// The token and the base live in `./credentials`, which is a seam rather than four calls to
// `localStorage`: a native shell keeps a device credential in the OS credential store, and a browser
// keeps it where a browser can. Re-exported here so the screens that already import `deviceToken`
// from this module are unchanged.
export { deviceToken } from "./credentials";

// The headers every call carries: JSON content-type when there is a body, and the device bearer token
// when one is stored (ADR-0084).
function authHeaders(hasBody: boolean): Record<string, string> {
  const headers: Record<string, string> = {};
  if (hasBody) {
    headers["content-type"] = "application/json";
  }
  const token = deviceToken();
  if (token !== null) {
    headers["authorization"] = `Bearer ${token}`;
  }
  return headers;
}

// `base` defaults to the edge this device paired with. Pairing itself passes one explicitly,
// because at that moment nothing is stored yet — the base is what the operator just supplied, and
// storing it before the call succeeds would leave a device claiming an edge that refused it.
async function request<T>(
  method: string,
  path: string,
  body?: unknown,
  base: string = edgeBase(),
): Promise<T> {
  const response = await fetch(base + path, {
    method,
    headers: authHeaders(body !== undefined),
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  // Which release answered (ADR-0111). Before the `ok` check on purpose: the call that just failed
  // is exactly the one an operator is looking at when they ask which version this box is running.
  observeEdgeVersion(response);
  observeLeaseStanding(response);
  if (!response.ok) {
    // A `401` means the device token is stale or missing — this device must pair again, so drop the
    // token and let the app route to pairing. A `403` means the token is fine but nobody is signed in
    // (S0b): keep the token, and the app routes to sign-in instead.
    if (response.status === 401) {
      clearDeviceToken();
    }
    const text = await response.text().catch(() => "");
    throw new ApiError(
      response.status,
      text.trim() || response.statusText,
      response.headers.get("pos-error-reason"),
    );
  }
  // A `204` has nothing to read, and reading nothing as JSON throws: retiring a device, and sending
  // or refusing a guest's order, each reported "the store did not respond" after the store had done
  // it. Releasing a till answers `204` too.
  if (response.status === 204) {
    return undefined as T;
  }
  return (await response.json()) as T;
}

// Who is signed in on this device, as `GET /api/session` reports it (S0b, ADR-0084) — and whether a
// replacement machine has taken this store (ADR-0123).
//
// The standing is on this response as well as on the `pos-lease-standing` header of every answer,
// because this is the call the app makes before it draws anything: a superseded till should say so
// on its sign-in screen, before anyone tries to seat a table.
export interface SessionState {
  signed_in: boolean;
  employee_id?: string;
  lease_standing: LeaseStanding;
  // Whether anyone can sign in on this box: the console has published at least one member of staff
  // with a PIN. Absent from an edge too old to send it, which the sign-in screen reads as "ready",
  // because a hint that the store has no staff must never be shown on a guess.
  sign_in_ready?: boolean;
  // How many seconds an attended till may sit untouched before it locks, from the store's
  // `session.idle_lock_seconds` (ADR-0160); `0` never locks. Absent from an edge too old to send
  // it, which never locks either.
  idle_lock_seconds?: number;
  // The float the Shift screen fills in when a shift opens, in the store currency's minor unit, from
  // the store's `shift.opening_float_minor` (ADR-0160); `0` fills in nothing. Absent from an edge
  // too old to send it, which fills in nothing either.
  opening_float_minor?: number;
  // Whether the store decides with each person's own permissions (ADR-0158 decision 6). Absent or
  // false: every control works as before, whatever the two lists below say.
  permissions_enforced?: boolean;
  // The permission ids the signed-in person holds directly, and those they hold only with another
  // person's approval. Empty when nobody is signed in; absent from an edge too old to send them.
  // The edge stays the authority: it refuses what the person may not do whatever the till shows.
  permissions?: string[];
  permissions_with_approval?: string[];
}

// The outcome of a sign-in attempt: the signed-in employee, or a refusal the screen can explain
// (a wrong code/PIN, or a lockout with the instant it lifts). Never leaks whether a code exists.
export type SignInResult =
  | { ok: true; employeeId: string }
  | { ok: false; outcome: "wrong" | "locked_out"; remaining?: number; untilMs?: number };

/**
 * A refused sign-in read out of a response body, or `undefined` when the body is not one.
 *
 * The edge answers **`401` for two opposite things** on this route: the paired gate refusing a device
 * that must pair again, and the sign-in check refusing a badge code or PIN
 * (`crates/pos-edge/src/http/auth.rs`). The status alone cannot separate them, so the body does — a
 * refusal is the JSON `SignInRefused` shape, the gate sends plain text.
 *
 * Deliberately strict: only the two outcomes the edge actually sends count. Anything else is treated
 * as *not* a refusal, because guessing the other way would swallow a real unpairing and strand the
 * operator on a screen whose every call then fails.
 */
function signInRefusal(body: string): SignInResult | undefined {
  let parsed: unknown;
  try {
    parsed = JSON.parse(body);
  } catch {
    return undefined;
  }
  if (typeof parsed !== "object" || parsed === null) {
    return undefined;
  }
  const refusal = parsed as { outcome?: unknown; remaining?: unknown; locked_until_ms?: unknown };
  if (refusal.outcome !== "wrong" && refusal.outcome !== "locked_out") {
    return undefined;
  }
  return {
    ok: false,
    outcome: refusal.outcome,
    remaining: typeof refusal.remaining === "number" ? refusal.remaining : undefined,
    untilMs: typeof refusal.locked_until_ms === "number" ? refusal.locked_until_ms : undefined,
  };
}

export const api = {
  seatTable: (tableId: string) =>
    request<TableResponse>("POST", `/api/tables/${tableId}/seat`),
  cleanTable: (tableId: string) =>
    request<TableResponse>("POST", `/api/tables/${tableId}/clean`),
  // A table seated by mistake goes straight back to the floor, while nothing is sold on it
  // (ADR-0163).
  releaseTable: (tableId: string) =>
    request<TableResponse>("POST", `/api/tables/${tableId}/release`),
  // Moves the guests at a table, and their order, to a free one; the table they left waits to be
  // cleared (`sales.table.transferred`).
  transferTable: (tableId: string, toTableId: string) =>
    request<TransferResponse>("POST", `/api/tables/${tableId}/transfer`, {
      to_table_id: toTableId,
    }),
  getTable: (tableId: string) => request<TableResponse>("GET", `/api/tables/${tableId}`),

  // The store's published floor plan and kitchen stations (ADR-0072). The app reads this at start to
  // draw the store's real tables and resolve fires to the store's default station.
  floor: () => request<FloorResponse>("GET", "/api/floor"),

  // What a table owes right now, assembled by the edge (roadmap-v3 E5). The till reads this rather
  // than adding up lines and applying a tax rate of its own — the figure shown to the guest and the
  // figure the bill settles against are then the same calculation.
  check: (tableId: string) => request<CheckResponse>("GET", `/api/tables/${tableId}/check`),

  // The store's published price book (roadmap-v3 E5, ADR-0063). Empty until the cloud publishes a
  // menu — a store never guesses a price, and neither does the till. Naming a channel reads that
  // channel's own book, the one the edge prices its orders from (ADR-0066); an edge that predates
  // the parameter ignores it and serves its own, which is also what it prices from.
  menu: (channel?: string) =>
    request<MenuResponse>(
      "GET",
      channel === undefined ? "/api/menu" : `/api/menu?channel=${encodeURIComponent(channel)}`,
    ),

  // Staff mark an item sold out at this store (86), and bring it back. Every device folds the
  // event and greys the item out; the edge refuses a line for it in the meantime.
  markSoldOut: (menuItemId: string) =>
    request<{ menu_item_id: string; sold_out: boolean }>("POST", `/api/menu/${menuItemId}/sold-out`),
  restoreItem: (menuItemId: string) =>
    request<{ menu_item_id: string; sold_out: boolean }>("POST", `/api/menu/${menuItemId}/restore`),

  // How the till groups and orders those items, from the `layout` node published beside the price
  // book (ADR-0066, production-readiness C4). A separate node, so a separate read: a price change
  // relays no buttons and a button moving reprices nothing.
  layout: () => request<LayoutResponse>("GET", "/api/layout"),

  // The money facts the pay pad needs: which notes a guest can hand over, and what the total rounds
  // to in cash (ADR-0105). A country's coinage, published rather than compiled into this app.
  locale: () => request<LocaleResponse>("GET", "/api/locale"),

  addLine: (tableId: string, line: LineRequest) =>
    request<LineResponse>("POST", `/api/tables/${tableId}/lines`, line),
  setLineQuantity: (lineId: string, body: QuantityRequest) =>
    request<LineResponse>("POST", `/api/lines/${lineId}/quantity`, body),

  fireLine: (lineId: string, fire: FireRequest) =>
    request<LineResponse>("POST", `/api/lines/${lineId}/fire`, fire),

  // Send every unsent line on an order in one transaction. The operator's act is "send this order",
  // and the per-line route made it one tap and one round trip each — with half the order reaching
  // the kitchen when one of them failed.
  fireOrder: (orderId: string, fire: FireRequest) =>
    request<LineResponse[]>("POST", `/api/orders/${orderId}/fire`, fire),
  // "Starters away" — the same act narrowed to one course (ADR-0130). A separate route rather than
  // a field on the body, because one of the two is gated and burying that in an optional field
  // would make whether the store needs `courses_enabled` depend on what was in the JSON.
  fireCourse: (orderId: string, courseId: string, fire: FireRequest) =>
    request<LineResponse[]>("POST", `/api/orders/${orderId}/fire/${courseId}`, fire),
  bumpTicket: (bump: BumpRequest) =>
    request<BumpResponse>("POST", "/api/kds/bump", bump),

  // The guest orders waiting for a member of staff, and the reasons a refusal may cite
  // (ADR-0116). Before these three the hold was reported and never enforced — there was nothing
  // for anyone to press.
  awaitingConfirmation: () =>
    request<WaitingResponse>("GET", "/api/orders/awaiting-confirmation"),
  confirmOrder: (orderId: string) =>
    request<void>("POST", `/api/orders/${orderId}/confirm`),
  rejectOrder: (orderId: string, reasonCodeId: string) =>
    request<void>("POST", `/api/orders/${orderId}/reject`, {
      reason_code_id: reasonCodeId,
    }),

  // The store's managed reason list (ADR-0115) — every active entry, each tagged with the actions
  // it may be cited for. Read once at start beside the price book: a picker that had to fetch its
  // own reasons would open empty for as long as the round-trip took, on a screen where the operator
  // is already mid-act.
  reasonCodes: () => request<ReasonCodesResponse>("GET", "/api/reason-codes"),

  // Void a line, citing a reason the store holds for voiding. The same request either way: the edge
  // decides whether a manager is needed (a fired line) or not (an ordinary cancel), so the till does
  // not have to encode the rule twice. A `403` means the approval is missing or was refused.
  voidLine: (lineId: string, request_: VoidRequest) =>
    request<LineResponse>("POST", `/api/lines/${lineId}/void`, request_),

  // Void a bill before it settles. Always needs a manager: a bill is money whether or not the
  // kitchen started. A settled one is refused with a 409 — reversing that is a refund.
  voidBill: (billId: string, request_: VoidRequest) =>
    request<VoidBillResponse>("POST", `/api/bills/${billId}/void`, request_),

  // Money off a bill before it settles. The response is the whole re-priced bill rather than an
  // acknowledgement, so the screen never subtracts a discount itself and arrives at a different tax.
  discountBill: (billId: string, request_: DiscountRequest) =>
    request<DiscountResponse>("POST", `/api/bills/${billId}/discount`, request_),

  // One fee off one bill (ADR-0159 decision 5). The answer is the bill as it now stands, as the
  // discount's is, so the screen never takes the fee off itself and misses its tax.
  waiveFee: (billId: string, feeId: string, request_: WaiveFeeRequest) =>
    request<CheckResponse>("POST", `/api/bills/${billId}/fees/${feeId}/waive`, request_),

  // Every counter order still owing money (ADR-0093) — the counter's equivalent of the floor plan.
  // A takeaway order is tableless by design, so without this a cashier would have to be told a ULID
  // to charge one.
  openOrders: () => request<CounterOrder[]>("GET", "/api/orders/open"),
  // The counter starts its own order (ADR-0146): a tableless order and its queue number, on
  // `channel` where the cashier asked the guest, and on the store's walk-in channel without one
  // (ADR-0160 decision 2).
  openOrder: (channel?: string) =>
    request<OpenedOrder>("POST", "/api/orders", channel === undefined ? {} : { channel }),
  addOrderLine: (orderId: string, line: OrderLineRequest) =>
    request<LineResponse>("POST", `/api/orders/${orderId}/lines`, line),

  // What is open right now, with the line ids to act on it. The read a device has instead of the
  // fan-out events it was not running to hear: without it a reloaded till draws an empty order and
  // a kitchen display switched on mid-service draws an empty board.
  liveOrders: () => request<LiveOrder[]>("GET", "/api/orders/live"),

  // What the kitchen still has to make (ADR-0161): the live orders, plus each paid order with no
  // table whose food no station has bumped yet. A counter order is paid before it is cooked.
  kitchenOrders: () => request<LiveOrder[]>("GET", "/api/orders/kitchen"),

  // What an order owes, for an order that sits on no table.
  checkOrder: (orderId: string) =>
    request<CheckResponse>("GET", `/api/orders/${orderId}/check`),

  openBill: (tableId: string) =>
    request<BillResponse>("POST", `/api/tables/${tableId}/bill`),

  // Open a bill on an order rather than a table — the counter's path to payment (ADR-0093).
  openBillForOrder: (orderId: string) =>
    request<BillResponse>("POST", `/api/orders/${orderId}/bill`),
  settleBill: (billId: string, settle: SettleRequest) =>
    request<BillResponse>("POST", `/api/bills/${billId}/settle`, settle),
  // Today's settled bills, newest first, and a copy of one's receipt: the original under the same
  // number, marked COPY, and counted (ADR-0164). A bill that has not settled has no receipt to copy.
  settledBills: () => request<SettledBill[]>("GET", "/api/bills/settled"),
  // What the store has taken today (ADR-0160): `403` unless the person's own role grants
  // `reports.takings.view`, whether or not the store enforces each person's own set.
  takings: () => request<TakingsResponse>("GET", "/api/reports/takings"),
  reprintReceipt: (billId: string) =>
    request<ReprintResponse>("POST", `/api/bills/${billId}/receipt/reprint`),

  // One bill by its id: what it owes, where it has got to and the lines it covers. What the pay
  // screen reads once a table's bill is split, because the table's own check answers for every part.
  billCheck: (billId: string) =>
    request<BillCheckResponse>("GET", `/api/bills/${billId}/check`),
  // The same figures on paper before payment: a pre-bill, unnumbered because nothing has settled
  // (roadmap-v3 B2.1). By bill for the guest paying one part of a split table; by table or order for
  // every open part at once. One outcome per piece of paper, in the order they printed.
  printBillCheck: (billId: string) =>
    request<PrintResponse>("POST", `/api/bills/${billId}/check/print`),
  printTableCheck: (tableId: string) =>
    request<PrintResponse>("POST", `/api/tables/${tableId}/check/print`),
  printOrderCheck: (orderId: string) =>
    request<PrintResponse>("POST", `/api/orders/${orderId}/check/print`),

  // Splitting a bill into parts, and folding parts back together (ADR-0128). Neither carries a PIN:
  // both move amounts already captured, and nothing is created, forgiven or taken out of the store.
  splitBill: (billId: string, split: SplitRequest) =>
    request<SplitResponse>("POST", `/api/bills/${billId}/split`, split),
  mergeBills: (billId: string, merge: MergeRequest) =>
    request<{ bill_id: string }>("POST", `/api/bills/${billId}/merge`, merge),

  openShift: (open: OpenShiftRequest) =>
    request<ShiftResponse>("POST", "/api/shifts", open),
  // The shift open now, or null (F4): what lets a device that reloaded count and close it. Where the
  // store keeps a drawer per till, this device's own till's.
  currentShift: () => request<ShiftResponse | null>("GET", "/api/shifts/current"),
  // Every drawer the store keeps, the model it runs, and which drawer is this device's (ADR-0167).
  drawers: () => request<DrawersResponse>("GET", "/api/shifts"),
  // The cloud link and the outbox, for the status bar (ADR-0137).
  sync: () => request<SyncResponse>("GET", "/api/sync"),
  // The vendor connections the cloud published to this store (ADR-0153).
  integrations: () => request<IntegrationEntry[]>("GET", "/api/integrations"),
  // The printers the store published, and a manager's test page on one.
  printers: () => request<PrinterEntry[]>("GET", "/api/printers"),
  testPrinter: (deviceId: string) =>
    request<TestPrintResponse>("POST", `/api/printers/${deviceId}/test`),
  // The store's tills, and which of them this device, another device or none is (ADR-0112), with
  // the till this device is when the store does not list it: what the Devices screen's *This device*
  // card shows. `403` unless the signed-in person's own role grants `admin.device.manage`, whether
  // or not the store enforces each person's own set.
  terminals: () => request<TerminalsResponse>("GET", "/api/print/agent"),
  // Makes this device that till, exclusively. `200` for each of the three outcomes, which are
  // answers about the store rather than faults in the request, so the screen says which.
  bindTerminal: (agentDeviceId: string) =>
    request<BindResponse>("POST", "/api/print/agent", { agent_device_id: agentDeviceId }),
  // Stops this device being that till: `204` whether or not it was, so a release never says what
  // another device holds.
  releaseTerminal: (agentDeviceId: string) =>
    request<void>("POST", "/api/print/agent/revoke", { agent_device_id: agentDeviceId }),
  countShift: (shiftId: string, count: CountShiftRequest) =>
    request<ShiftResponse>("POST", `/api/shifts/${shiftId}/count`, count),
  closeShift: (shiftId: string) =>
    request<ShiftResponse>("POST", `/api/shifts/${shiftId}/close`),
  // Cash paid in or out of the drawer outside a sale, and the drawer opened without one (ADR-0165).
  paidIn: (shiftId: string, movement: CashMovementRequest) =>
    request<ShiftResponse>("POST", `/api/shifts/${shiftId}/paid-in`, movement),
  paidOut: (shiftId: string, movement: CashMovementRequest) =>
    request<ShiftResponse>("POST", `/api/shifts/${shiftId}/paid-out`, movement),
  openDrawer: (open: OpenDrawerRequest) =>
    request<OpenDrawerResponse>("POST", "/api/drawer/open", open),

  // Redeem a pairing code for a device token, and keep the token **and the edge it came from** so
  // every later call carries the one against the other (ADR-0084, ADR-0111). Pairing itself is
  // unauthenticated — it is how a device obtains the token.
  //
  // `base` is empty for a till served by the box it is pairing with, which is every in-store device
  // and is what keeps its requests root-relative. A shell pairing against a named edge passes that
  // origin, and it is stored beside the token because a token is only meaningful against the edge
  // that issued it.
  pair: async (code: string, base = ""): Promise<PairAccepted> => {
    const accepted = await request<PairAccepted>("POST", "/api/pair", { code }, base);
    rememberPairing(base, accepted.device_token);
    return accepted;
  },

  // Which devices this store has admitted (ADR-0091, production-readiness O1). Behind the paired
  // gate: a device that is itself paired may list and retire the others, which is as strong as
  // pairing and no stronger — the edge has no operator identity offline.
  pairedDevices: () => request<PairingState>("GET", "/api/pair/devices"),

  // Retire a device, or every device when `deviceId` is null — the break-glass that re-pairs the
  // whole store. Answers `204`; a `503` means the durable registry could not be written, so the
  // device may still be paired after a restart and the screen must not claim otherwise.
  revokeDevice: (deviceId: string | null) =>
    request<void>("POST", "/api/pair/revoke", deviceId === null ? {} : { device_id: deviceId }),

  // Mint the pairing code for the next device (ADR-0118). Behind **both** gates — this till must be
  // paired *and* a manager must be signed in on it — because issuing a credential is a stronger act
  // than retiring one. A `403` means the signed-in person lacks the permission, not that the till
  // needs to pair again.
  //
  // Replaces whatever code was live, including the one the store server minted at boot. The reply is
  // the only copy: show it, and mint again if it is lost.
  mintPairingCode: () => request<MintedCode>("POST", "/api/pair/codes"),

  // Whether this store server holds its device credential yet (ADR-0050, ADR-0086). A store that is
  // not provisioned for a cloud does not mount the route at all, so a rejection here means "there is
  // nothing to activate", not "the store is broken" — the caller carries on to the counter, which
  // trades offline regardless (ADR-0001).
  activation: () => request<ActivationStanding>("GET", "/api/activation"),

  // What a box being claimed shows (ADR-0148): served by `pos-edge claim` alone, before the box has
  // a store, a store server or a paired device. Unauthenticated, and loopback-only on the box.
  claimStatus: () => request<ClaimStatus>("GET", "/api/claim"),

  // Exchange the activation code from the store's setup sheet for the box's device credential
  // (ADR-0050). Unauthenticated, like pairing: a fresh box holds no token yet. The credential stays
  // on the box — the answer carries the device id alone.
  activate: (code: string) => request<ActivateAccepted>("POST", "/api/activate", { code }),

  // Who (if anyone) is signed in on this device (S0b). Throws `isUnauthorized` if the device is not
  // paired, which the app treats the same as a missing token — route to pairing.
  session: () => request<SessionState>("GET", "/api/session"),

  // Sign a member of staff in with their badge code and PIN (S0b, ADR-0084). Returns a structured
  // result rather than throwing on a wrong PIN, so the screen can show the attempts left or the
  // lockout countdown. The PIN is sent once and never stored.
  signIn: async (code: string, pin: string): Promise<SignInResult> => {
    // Takes the base at its own call site, as `request()` does. Changing only `request()` would ship
    // a shell that can read the floor and settle a bill but can never sign an employee in — a worse
    // failure than not shipping one, since the three session routes are covered precisely so a second
    // origin can sign in (ADR-0111).
    const response = await fetch(edgeBase() + "/api/session/sign-in", {
      method: "POST",
      headers: authHeaders(true),
      body: JSON.stringify({ code, pin }),
    });
    observeEdgeVersion(response);
    observeLeaseStanding(response);
    if (response.ok) {
      const body = (await response.json()) as { employee_id: string };
      return { ok: true, employeeId: body.employee_id };
    }
    // The body decides, not the status: a refused sign-in and an unpaired device are both `401`, and
    // reading the status alone dropped the device token on every mistyped PIN — the tablet bounced
    // back to pairing, and the screen's three refusal messages could never be reached.
    const body = await response.text().catch(() => "");
    const refusal = signInRefusal(body);
    if (refusal !== undefined) {
      return refusal;
    }
    // Not a refusal, so a `401` is the paired gate: this device must pair again, which is the one
    // case that clears the token.
    if (response.status === 401) {
      clearDeviceToken();
      throw new ApiError(401, "pair this device to reach the edge");
    }
    return { ok: false, outcome: "wrong" };
  },

  // Sign the current employee out on this device (S0b). The device stays paired; the next command
  // needs a fresh sign-in.
  signOut: async (): Promise<void> => {
    const response = await fetch(edgeBase() + "/api/session/sign-out", {
      method: "POST",
      headers: authHeaders(false),
    });
    // The two session calls bypass `request()` on purpose — one reads a structured refusal, the
    // other wants no body at all — so each observes the header at its own call site. A client that
    // only stamped `request()` would learn nothing from the three routes a second origin most needs.
    observeEdgeVersion(response);
    observeLeaseStanding(response);
  },
};
