// The typed HTTP client for the edge's domain routes. Every call is one `fetch` to `base() + path`,
// where the base is empty for a till served by the box it talks to — so an in-store device sends the
// identical root-relative request it always has, and a native shell or a hosted placement sends the
// same path against the origin it paired with (ADR-0111). A refused command comes back as a non-2xx
// with a plain-text reason (the edge maps a domain refusal to 409); this surfaces it as an
// `ApiError` the screens can show without guessing.

import { clearDeviceToken, deviceToken, edgeBase, rememberPairing } from "./credentials";
import { observeEdgeVersion } from "./edgeVersion";
import type {
  ActivateAccepted,
  ActivationStanding,
  BillResponse,
  BumpRequest,
  BumpResponse,
  CheckResponse,
  CountShiftRequest,
  CounterOrder,
  FireRequest,
  FloorResponse,
  LineRequest,
  LineResponse,
  LayoutResponse,
  LocaleResponse,
  MenuResponse,
  OpenShiftRequest,
  PairAccepted,
  PairingState,
  SettleRequest,
  ShiftResponse,
  TableResponse,
} from "./types";

export class ApiError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
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
  if (!response.ok) {
    // A `401` means the device token is stale or missing — this device must pair again, so drop the
    // token and let the app route to pairing. A `403` means the token is fine but nobody is signed in
    // (S0b): keep the token, and the app routes to sign-in instead.
    if (response.status === 401) {
      clearDeviceToken();
    }
    const text = await response.text().catch(() => "");
    throw new ApiError(response.status, text.trim() || response.statusText);
  }
  return (await response.json()) as T;
}

// Who is signed in on this device, as `GET /api/session` reports it (S0b, ADR-0084).
export interface SessionState {
  signed_in: boolean;
  employee_id?: string;
}

// The outcome of a sign-in attempt: the signed-in employee, or a refusal the screen can explain
// (a wrong code/PIN, or a lockout with the instant it lifts). Never leaks whether a code exists.
export type SignInResult =
  | { ok: true; employeeId: string }
  | { ok: false; outcome: "wrong" | "locked_out"; remaining?: number; untilMs?: number };

export const api = {
  seatTable: (tableId: string) =>
    request<TableResponse>("POST", `/api/tables/${tableId}/seat`),
  cleanTable: (tableId: string) =>
    request<TableResponse>("POST", `/api/tables/${tableId}/clean`),
  getTable: (tableId: string) => request<TableResponse>("GET", `/api/tables/${tableId}`),

  // The store's published floor plan and kitchen stations (ADR-0072). The app reads this at start to
  // draw the store's real tables and resolve fires to the store's default station.
  floor: () => request<FloorResponse>("GET", "/api/floor"),

  // What a table owes right now, assembled by the edge (roadmap-v3 E5). The till reads this rather
  // than adding up lines and applying a tax rate of its own — the figure shown to the guest and the
  // figure the bill settles against are then the same calculation.
  check: (tableId: string) => request<CheckResponse>("GET", `/api/tables/${tableId}/check`),

  // The store's published price book (roadmap-v3 E5, ADR-0063). Empty until the cloud publishes a
  // menu — a store never guesses a price, and neither does the till.
  menu: () => request<MenuResponse>("GET", "/api/menu"),

  // How the till groups and orders those items, from the `layout` node published beside the price
  // book (ADR-0066, production-readiness C4). A separate node, so a separate read: a price change
  // relays no buttons and a button moving reprices nothing.
  layout: () => request<LayoutResponse>("GET", "/api/layout"),

  // The money facts the pay pad needs: which notes a guest can hand over, and what the total rounds
  // to in cash (ADR-0105). A country's coinage, published rather than compiled into this app.
  locale: () => request<LocaleResponse>("GET", "/api/locale"),

  addLine: (tableId: string, line: LineRequest) =>
    request<LineResponse>("POST", `/api/tables/${tableId}/lines`, line),
  fireLine: (lineId: string, fire: FireRequest) =>
    request<LineResponse>("POST", `/api/lines/${lineId}/fire`, fire),
  bumpTicket: (bump: BumpRequest) =>
    request<BumpResponse>("POST", "/api/kds/bump", bump),

  // Every counter order still owing money (ADR-0093) — the counter's equivalent of the floor plan.
  // A takeaway order is tableless by design, so without this a cashier would have to be told a ULID
  // to charge one.
  openOrders: () => request<CounterOrder[]>("GET", "/api/orders/open"),

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

  openShift: (open: OpenShiftRequest) =>
    request<ShiftResponse>("POST", "/api/shifts", open),
  countShift: (shiftId: string, count: CountShiftRequest) =>
    request<ShiftResponse>("POST", `/api/shifts/${shiftId}/count`, count),
  closeShift: (shiftId: string) =>
    request<ShiftResponse>("POST", `/api/shifts/${shiftId}/close`),

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

  // Whether this store server holds its device credential yet (ADR-0050, ADR-0086). A store that is
  // not provisioned for a cloud does not mount the route at all, so a rejection here means "there is
  // nothing to activate", not "the store is broken" — the caller carries on to the counter, which
  // trades offline regardless (ADR-0001).
  activation: () => request<ActivationStanding>("GET", "/api/activation"),

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
    if (response.ok) {
      const body = (await response.json()) as { employee_id: string };
      return { ok: true, employeeId: body.employee_id };
    }
    // A `401` here means the device itself is unpaired (the paired gate, not the sign-in check); let
    // it surface so the app routes to pairing.
    if (response.status === 401) {
      clearDeviceToken();
      throw new ApiError(401, "pair this device to reach the edge");
    }
    const refusal = (await response
      .json()
      .catch(() => ({}))) as {
      outcome?: "wrong" | "locked_out";
      remaining?: number;
      locked_until_ms?: number;
    };
    return {
      ok: false,
      outcome: refusal.outcome ?? "wrong",
      remaining: refusal.remaining,
      untilMs: refusal.locked_until_ms,
    };
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
  },
};
