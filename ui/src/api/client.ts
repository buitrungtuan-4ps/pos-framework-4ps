// The typed HTTP client for the edge's domain routes. Every call is one `fetch` to the same origin
// that served the app, so it works on the store LAN with no configuration. A refused command comes
// back as a non-2xx with a plain-text reason (the edge maps a domain refusal to 409); this surfaces
// it as an `ApiError` the screens can show without guessing.

import type {
  BillResponse,
  BumpRequest,
  BumpResponse,
  CountShiftRequest,
  FireRequest,
  FloorResponse,
  LineRequest,
  LineResponse,
  OpenShiftRequest,
  PairAccepted,
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
}

// The bearer token a device was issued when it paired (ADR-0084). Kept in localStorage so it
// survives a reload; every wrapper is defensive because a private window or blocked storage throws.
const DEVICE_TOKEN_KEY = "pos-edge.device-token";

export function deviceToken(): string | null {
  try {
    return localStorage.getItem(DEVICE_TOKEN_KEY);
  } catch {
    return null;
  }
}

function setDeviceToken(token: string): void {
  try {
    localStorage.setItem(DEVICE_TOKEN_KEY, token);
  } catch {
    // A device that cannot persist its token re-pairs each session; that is degraded, not broken.
  }
}

function clearDeviceToken(): void {
  try {
    localStorage.removeItem(DEVICE_TOKEN_KEY);
  } catch {
    // Nothing to do — the next domain call will 401 and route the operator to pair anyway.
  }
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = {};
  if (body !== undefined) {
    headers["content-type"] = "application/json";
  }
  const token = deviceToken();
  if (token !== null) {
    headers["authorization"] = `Bearer ${token}`;
  }
  const response = await fetch(path, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) {
    // A stale or missing token means this device must pair again; drop it so the app can route there.
    if (response.status === 401) {
      clearDeviceToken();
    }
    const text = await response.text().catch(() => "");
    throw new ApiError(response.status, text.trim() || response.statusText);
  }
  return (await response.json()) as T;
}

export const api = {
  seatTable: (tableId: string) =>
    request<TableResponse>("POST", `/api/tables/${tableId}/seat`),
  cleanTable: (tableId: string) =>
    request<TableResponse>("POST", `/api/tables/${tableId}/clean`),
  getTable: (tableId: string) => request<TableResponse>("GET", `/api/tables/${tableId}`),

  // The store's published floor plan and kitchen stations (ADR-0072). The app reads this at start to
  // draw the store's real tables and resolve fires to the store's default station.
  floor: () => request<FloorResponse>("GET", "/api/floor"),

  addLine: (tableId: string, line: LineRequest) =>
    request<LineResponse>("POST", `/api/tables/${tableId}/lines`, line),
  fireLine: (lineId: string, fire: FireRequest) =>
    request<LineResponse>("POST", `/api/lines/${lineId}/fire`, fire),
  bumpTicket: (bump: BumpRequest) =>
    request<BumpResponse>("POST", "/api/kds/bump", bump),

  openBill: (tableId: string) =>
    request<BillResponse>("POST", `/api/tables/${tableId}/bill`),
  settleBill: (billId: string, settle: SettleRequest) =>
    request<BillResponse>("POST", `/api/bills/${billId}/settle`, settle),

  openShift: (open: OpenShiftRequest) =>
    request<ShiftResponse>("POST", "/api/shifts", open),
  countShift: (shiftId: string, count: CountShiftRequest) =>
    request<ShiftResponse>("POST", `/api/shifts/${shiftId}/count`, count),
  closeShift: (shiftId: string) =>
    request<ShiftResponse>("POST", `/api/shifts/${shiftId}/close`),

  // Redeem a pairing code for a device token, and keep the token so every later domain call carries
  // it (ADR-0084). Pairing itself is unauthenticated — it is how a device obtains the token.
  pair: async (code: string): Promise<PairAccepted> => {
    const accepted = await request<PairAccepted>("POST", "/api/pair", { code });
    setDeviceToken(accepted.device_token);
    return accepted;
  },
};
