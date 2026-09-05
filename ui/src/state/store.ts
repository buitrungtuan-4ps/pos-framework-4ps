// The client's small projection: the floor, the order lines on each table, the open bill per table,
// and the shift — folded from the same fan-out events the edge publishes (ADR-0018), so what one
// device does appears on every other. Actions call the typed client and lean on the fan-out to
// reconcile; a line the operator adds shows at once and is de-duplicated when its own event returns.

import { createStore, produce } from "solid-js/store";

import { api } from "../api/client";
import type { LinkStatus, ServerEvent } from "../api/live";
import type {
  BillResponse,
  CheckResponse,
  LineRequest,
  LayoutCategory,
  MenuItemResponse,
  PaymentRequest,
} from "../api/types";
import { fallbackQuickCash, type Money } from "../lib/money";

export interface OrderLine {
  orderLineId: string;
  orderId: string;
  name: string;
  quantityMilli: number;
  lineTotal: Money;
  state: string;
}

export interface TableCard {
  id: string;
  label: string;
  state: string;
}

export interface ShiftInfo {
  shiftId: string;
  state: string;
  expected?: Money;
  counted?: Money;
  variance?: Money;
}

interface StoreShape {
  link: LinkStatus;
  // The floor the app draws — the store's published tables once `loadFloor` syncs them, and a
  // sensible default until it does (the never-blank config contract, ADR-0072).
  floor: TableCard[];
  // The station a fire and a bump route to when no per-item rule applies — the store's published
  // default station once synced, and a bootstrap fallback until then. The edge re-derives a fired
  // line's station from the published routing, so this is only the fallback the caller carries.
  defaultStation: string;
  tableState: Record<string, string>;
  tableOrder: Record<string, string>;
  orderTable: Record<string, string>;
  lines: Record<string, OrderLine>;
  openBill: Record<string, string>;
  // The store's own price book, from `GET /api/menu` (roadmap-v3 E5, ADR-0063). Empty until the edge
  // serves it — a store never guesses a price, so the till shows nothing to sell rather than a list
  // compiled into the app.
  menu: MenuItemResponse[];
  // How the till groups and orders those items, from `GET /api/layout` (ADR-0066, C4). Empty means
  // the console has laid nothing out — the till then draws the flat price book, which is what it drew
  // before the `layout` node had a reader at all.
  layout: LayoutCategory[];
  // The store's currency, from the same `GET /api/menu` read as the price book (roadmap-v3 E5). The
  // edge is the authority: it comes from the synced `locale` node (ADR-0074), which a store outside
  // Vietnam sets to its own. `null` until the price book loads — a screen that needs a currency
  // before then has no answer, and `storeCurrency()` says which fallback it uses and why.
  currency: string | null;
  // The two published store facts the till has to obey, from the same `GET /api/menu` read as the
  // price book. `tipsEnabled` gates the tip entry; `acceptedTender` gates the tender buttons.
  // `null` for either means "the price book has not loaded yet"; for `acceptedTender` a loaded
  // `null` means the store restricts nothing, which is why the accessor below distinguishes them.
  tipsEnabled: boolean;
  acceptedTender: string[] | null;
  // The notes this store's guests carry, from `GET /api/locale` (ADR-0105). `null` means the locale
  // read has not landed; an empty array is a real answer and means "the exact amount only". The
  // difference matters, because the fallback below applies to the first and not the second.
  cashDenominations: number[] | null;
  // Lines a station has marked prepared (`kitchen.ticket.bumped`). Folded from the fan-out, not held
  // per-screen, so every KDS agrees a ticket is done (#44).
  bumped: Record<string, boolean>;
  shift: ShiftInfo | null;
}

const tid = (code: string): string => code.padStart(26, "0");

// The fallback floor the app draws before the store's real layout syncs from config, and if a store
// has none published (never-blank, ADR-0072). Replaced wholesale by `loadFloor` once the edge serves
// the published plan.
const DEFAULT_FLOOR: readonly TableCard[] = Array.from({ length: 8 }, (_, index) => {
  const number = index + 1;
  return { id: tid(`T${number.toString().padStart(2, "0")}`), label: String(number), state: "TABLE_STATE_FREE" };
});

// The fallback station a fire/bump carries until the store publishes a station plan (never-blank).
const DEFAULT_STATION = tid("S01");

const [state, setState] = createStore<StoreShape>({
  link: "connecting",
  floor: DEFAULT_FLOOR.map((table) => ({ ...table })),
  defaultStation: DEFAULT_STATION,
  tableState: Object.fromEntries(DEFAULT_FLOOR.map((table) => [table.id, table.state])),
  tableOrder: {},
  orderTable: {},
  lines: {},
  openBill: {},
  menu: [],
  layout: [],
  currency: null,
  // Closed until the edge says otherwise: a till that offers a tip the store does not take is worse
  // than one that waits a moment for the price book.
  tipsEnabled: false,
  acceptedTender: null,
  cashDenominations: null,
  bumped: {},
  shift: null,
});

export { state };

// The floor the app draws — reactive, so a screen's `<For>` re-renders when `loadFloor` syncs the
// store's real tables.
export function floorTables(): readonly TableCard[] {
  return state.floor;
}

// Reads the store's published floor plan and kitchen stations from the edge (ADR-0072) and folds them
// in: the real tables replace the default grid, and the plan's default station replaces the bootstrap
// fallback. Forgiving and never-blank — an empty plan or a failed read leaves the fallback in place,
// so the floor is never wiped out from under the operator.
export async function loadFloor(): Promise<void> {
  let response;
  try {
    response = await api.floor();
  } catch {
    // The route may be briefly unavailable (a device that just paired, the edge still starting);
    // keep whatever floor we hold and let a later reload pick it up.
    return;
  }
  const tables: TableCard[] = [];
  for (const area of response.floor.areas ?? []) {
    for (const table of area.tables ?? []) {
      tables.push({
        id: table.table_id,
        label: table.label,
        state: state.tableState[table.table_id] ?? "TABLE_STATE_FREE",
      });
    }
  }
  const defaultStation = response.stations.default_station_id ?? undefined;
  setState(
    produce((draft) => {
      if (tables.length > 0) {
        draft.floor = tables;
        // Rebuild the table-state map for the synced tables, preserving any live state already folded
        // from the fan-out and defaulting new tables to free.
        draft.tableState = Object.fromEntries(
          tables.map((table) => [table.id, draft.tableState[table.id] ?? "TABLE_STATE_FREE"]),
        );
      }
      if (defaultStation !== undefined) {
        draft.defaultStation = defaultStation;
      }
    }),
  );
}

// ---- reading ----------------------------------------------------------------

export function tableState(tableId: string): string {
  return state.tableState[tableId] ?? "TABLE_STATE_FREE";
}

export function linesForTable(tableId: string): OrderLine[] {
  const orderId = state.tableOrder[tableId];
  if (orderId === undefined) {
    return [];
  }
  return Object.values(state.lines).filter((line) => line.orderId === orderId);
}

export function openBillFor(tableId: string): string | undefined {
  return state.openBill[tableId];
}

// The bill total the operator collects, computed client-side from the captured line totals and the
// bootstrap's single 10% standard rate — the same arithmetic the edge does for one tax class. Tax
// What the table owes right now, asked of the edge (roadmap-v3 E5). The till used to add the lines up
// itself and apply a tax rate hardcoded at 10%, so a store on any other rate — or with more than one
// tax class — showed the guest one number and settled against another. The edge assembles this with
// the same `billing::assemble` the settle path runs, so there is one calculation, in the domain.
export async function loadCheck(tableId: string): Promise<CheckResponse> {
  return api.check(tableId);
}

// ---- small payload readers (fan-out payloads are untyped JSON) --------------

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : null;
}

function str(source: Record<string, unknown>, key: string): string | null {
  const value = source[key];
  return typeof value === "string" ? value : null;
}

function asMoney(value: unknown): Money | null {
  const object = record(value);
  if (object === null) {
    return null;
  }
  const currency = str(object, "currency_code");
  const amount = object["amount_minor"];
  if (currency === null || typeof amount !== "number") {
    return null;
  }
  return { currency_code: currency, amount_minor: amount };
}

// ---- folding the fan-out ----------------------------------------------------

export function setLink(status: LinkStatus): void {
  setState("link", status);
}

export function fold(event: ServerEvent): void {
  const payload = record(event.payload);
  if (payload === null) {
    return;
  }
  switch (event.eventType) {
    case "sales.table.opened": {
      const table = str(payload, "table_id");
      const order = str(payload, "order_id");
      if (table !== null && order !== null) {
        setState(
          produce((draft) => {
            draft.tableState[table] = "TABLE_STATE_OCCUPIED";
            draft.tableOrder[table] = order;
            draft.orderTable[order] = table;
          }),
        );
      }
      break;
    }
    case "sales.table.closed": {
      const table = str(payload, "table_id");
      if (table !== null) {
        setState("tableState", table, "TABLE_STATE_FREE");
      }
      break;
    }
    case "sales.order_line.added": {
      const line = readLine(payload);
      if (line !== null) {
        setState("lines", line.orderLineId, line);
      }
      break;
    }
    case "sales.order_line.fired": {
      const lineId = str(payload, "order_line_id");
      if (lineId !== null && state.lines[lineId] !== undefined) {
        setState("lines", lineId, "state", "ORDER_LINE_STATE_FIRED");
      }
      break;
    }
    case "kitchen.ticket.bumped": {
      const ids = payload["order_line_ids"];
      if (Array.isArray(ids)) {
        setState(
          produce((draft) => {
            for (const id of ids) {
              if (typeof id === "string") {
                draft.bumped[id] = true;
              }
            }
          }),
        );
      }
      break;
    }
    case "billing.bill.opened": {
      const bill = str(payload, "bill_id");
      const order = str(payload, "order_id");
      const table = order !== null ? state.orderTable[order] : undefined;
      if (bill !== null && table !== undefined) {
        setState(
          produce((draft) => {
            draft.openBill[table] = bill;
            draft.tableState[table] = "TABLE_STATE_AWAITING_PAYMENT";
          }),
        );
      }
      break;
    }
    case "billing.bill.settled": {
      const bill = str(payload, "bill_id");
      if (bill !== null) {
        const table = Object.keys(state.openBill).find((key) => state.openBill[key] === bill);
        if (table !== undefined) {
          setState(
            produce((draft) => {
              draft.tableState[table] = "TABLE_STATE_NEEDS_CLEANING";
              delete draft.openBill[table];
            }),
          );
        }
      }
      break;
    }
    default:
      break;
  }
}

function readLine(payload: Record<string, unknown>): OrderLine | null {
  const orderLineId = str(payload, "order_line_id");
  const orderId = str(payload, "order_id");
  const name = str(payload, "display_name");
  const lineTotal = asMoney(payload["line_total"]);
  const quantity = record(payload["quantity"]);
  const milli = quantity !== null && typeof quantity["milli"] === "number" ? quantity["milli"] : 1000;
  if (orderLineId === null || orderId === null || name === null || lineTotal === null) {
    return null;
  }
  return { orderLineId, orderId, name, quantityMilli: milli, lineTotal, state: "ORDER_LINE_STATE_ADDED" };
}

// The floor label for a table id (the "3" of table 3), for the kitchen and expo tickets.
export function tableLabel(tableId: string): string {
  return state.floor.find((table) => table.id === tableId)?.label ?? tableId;
}

export interface KitchenLine {
  orderLineId: string;
  orderId: string;
  name: string;
  tableLabel: string;
}

// Every fired line still on an open order, newest tables last — what the kitchen and the pass work
// from. A line whose table has been cleaned (its bill settled) drops out, because the order is done;
// a line a station has bumped (`kitchen.ticket.bumped`) drops out too, so a KDS coming online after
// the bump agrees the ticket is made rather than re-showing it.
export function firedLines(): KitchenLine[] {
  const liveOrders = new Set(Object.values(state.tableOrder));
  return Object.values(state.lines)
    .filter(
      (line) =>
        line.state === "ORDER_LINE_STATE_FIRED" &&
        liveOrders.has(line.orderId) &&
        state.bumped[line.orderLineId] !== true,
    )
    .map((line) => ({
      orderLineId: line.orderLineId,
      orderId: line.orderId,
      name: line.name,
      tableLabel: tableLabel(state.orderTable[line.orderId] ?? ""),
    }));
}

// Table counts by state, for the Today summary.
export function tableCounts(): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const table of state.floor) {
    const current = tableState(table.id);
    counts[current] = (counts[current] ?? 0) + 1;
  }
  return counts;
}

export function openBillCount(): number {
  return Object.keys(state.openBill).length;
}

// ---- commands (call the edge, update at once, let the fan-out reconcile) -----

export async function seat(tableId: string): Promise<void> {
  const response = await api.seatTable(tableId);
  setState("tableState", tableId, response.state);
}

export async function clean(tableId: string): Promise<void> {
  const response = await api.cleanTable(tableId);
  setState("tableState", tableId, response.state);
}

// Adds one of the store's own menu items to a table. Every amount on the line is the edge's, passed
// straight back rather than recomputed here: the price and the rate are what the store published and
// what the guest was shown (roadmap-v3 E5). An item the edge reported unavailable is refused before
// the round-trip — the button is disabled too, but the guard belongs with the action.
export async function addItem(tableId: string, item: MenuItemResponse): Promise<void> {
  if (!item.available || item.tax_rate === undefined || item.tax_rate === null) {
    throw new Error(`${item.display_name} is not sellable`);
  }
  const line: LineRequest = {
    menu_item_id: item.menu_item_id,
    display_name: item.display_name,
    quantity: { milli: 1000 },
    unit_price: item.unit_price,
    // One unit, so the line total is the unit price — the only quantity the till offers today, and
    // the edge is the authority on the figure either way.
    line_total: item.unit_price,
    tax_class_id: item.tax_class_id,
    tax_rate: item.tax_rate,
    note_present: false,
  };
  const response = await api.addLine(tableId, line);
  setState(
    produce((draft) => {
      draft.tableOrder[tableId] = response.order_id;
      draft.orderTable[response.order_id] = tableId;
      draft.lines[response.order_line_id] = {
        orderLineId: response.order_line_id,
        orderId: response.order_id,
        name: item.display_name,
        quantityMilli: 1000,
        lineTotal: item.unit_price,
        state: response.state,
      };
    }),
  );
}

// The currency every amount on screen is in, as the edge reported it with the price book.
//
// Until roadmap **E5** five places wrote `"VND"` as a literal — the shift's opening float, the
// shift screen's expected/counted figures, one Pay label, the Pay screen's quick-cash notes and the
// takeaway counter's — so a store outside Vietnam would
// have shown and *sent* the wrong currency code on its own cash count. The edge has always known
// the answer (`MenuResponse.currency`, from the synced `locale` node); the app simply threw it
// away.
//
// `DEFAULT_CURRENCY` is the never-blank fallback for the window before the price book loads, in
// keeping with `DEFAULT_FLOOR` and `DEFAULT_STATION`: a till that shows no currency at all is worse
// than one showing the deployment's own, and the value is replaced wholesale by the edge's the
// moment `loadMenu` succeeds. A fork ships its own here.
const DEFAULT_CURRENCY = "VND";

export function storeCurrency(): string {
  return state.currency ?? DEFAULT_CURRENCY;
}

// Whether this store takes tips, as the edge reported it with the price book (roadmap-v3 **B1.3**).
//
// The domain half of B1.3 shipped first: `Payment.tip` reaches the edge, `decide_bill` requires
// `Capability::Tips`, and the settled event records the amount. None of it could ever fire, because
// the till had no tip entry at all — so `tip_amount` was zero on every payment a real store took.
// This is the gate that field is behind.
export function tipsEnabled(): boolean {
  return state.tipsEnabled;
}

// Whether the store accepts `method` (a `PAYMENT_METHOD_*` wire name).
//
// An unloaded or unrestricted store accepts everything, which is why both are `true` rather than
// only the unrestricted one: refusing every tender while the price book loads would leave a till
// with no way to take money at all, and the edge is the authority that refuses either way.
export function tenderAccepted(method: string): boolean {
  const accepted = state.acceptedTender;
  return accepted === null || accepted.includes(method);
}

// Reads the store's published price book from the edge (ADR-0063) into the projection. Forgiving: a
// failed read leaves whatever is already loaded, so a blip does not empty the till mid-service. An
// empty menu is a real answer — the store has published none — and the screen says so.
export async function loadMenu(): Promise<void> {
  try {
    const response = await api.menu();
    setState("menu", response.items);
    setState("currency", response.currency);
    setState("tipsEnabled", response.tips_enabled);
    setState("acceptedTender", response.accepted_tender);
  } catch {
    // The counter keeps whatever it last loaded; the next boot or reload tries again.
  }
}

// The quick-cash keys the pay pad offers, in minor units, ascending (ADR-0105).
//
// The published list once `loadLocale` lands, and the compiled-in table for that currency until it
// does. The fallback is the same never-blank contract as `DEFAULT_FLOOR` and `DEFAULT_CURRENCY`: a
// till whose locale has not synced keeps the keys it had, and the published list replaces them
// wholesale — including with an empty list, which is a store saying "exact amount only" rather than
// a read that has not happened.
export function cashDenominations(): readonly number[] {
  return state.cashDenominations ?? fallbackQuickCash(storeCurrency());
}

// Reads the store's money settings from the edge (ADR-0105). Forgiving in the same way `loadMenu`
// is: a failed read leaves the previous keys in place, so a blip does not strand a cashier with one
// button mid-service.
export async function loadLocale(): Promise<void> {
  try {
    const response = await api.locale();
    setState("cashDenominations", response.cash_denominations);
  } catch {
    // Keep whatever is loaded; the next boot or reload tries again.
  }
}

// Reads the store's published button plan (ADR-0066, C4). Forgiving in the same way `loadMenu` is: a
// failed read leaves whatever is already arranged, and an empty plan is a real answer — the console
// has laid nothing out, and the screen falls back to the flat price book.
export async function loadLayout(): Promise<void> {
  try {
    const response = await api.layout();
    setState("layout", response.categories);
  } catch {
    // Keep the last arrangement; the next boot or reload tries again.
  }
}

export async function fire(lineId: string): Promise<void> {
  // The edge derives the fired line's station from the published routing (ADR-0072); the station we
  // send is only the fallback it uses when the store has published no station plan yet.
  const response = await api.fireLine(lineId, { station_id: state.defaultStation });
  setState("lines", lineId, "state", response.state);
}

// A station marks a ticket's lines prepared. The edge records the durable `kitchen.ticket.bumped`
// event and fans it out, so every KDS folds the same prepared set; the line drops off this screen at
// once and stays off when its own event returns.
export async function bump(orderId: string, orderLineIds: string[]): Promise<void> {
  await api.bumpTicket({
    order_id: orderId,
    station_id: state.defaultStation,
    order_line_ids: orderLineIds,
  });
  setState(
    produce((draft) => {
      for (const id of orderLineIds) {
        draft.bumped[id] = true;
      }
    }),
  );
}

export async function openBill(tableId: string): Promise<string> {
  const existing = state.openBill[tableId];
  if (existing !== undefined) {
    return existing;
  }
  const response = await api.openBill(tableId);
  setState(
    produce((draft) => {
      draft.openBill[tableId] = response.bill_id;
      if (response.table_state !== undefined) {
        draft.tableState[tableId] = response.table_state;
      }
    }),
  );
  return response.bill_id;
}

export async function settle(billId: string, payments: PaymentRequest[]): Promise<BillResponse> {
  const response = await api.settleBill(billId, { payments });
  const table = Object.keys(state.openBill).find((key) => state.openBill[key] === billId);
  if (table !== undefined) {
    setState(
      produce((draft) => {
        delete draft.openBill[table];
        if (response.table_state !== undefined) {
          draft.tableState[table] = response.table_state;
        }
      }),
    );
  }
  return response;
}

// ---- shift, driven by the operator's own commands ---------------------------

export async function openShift(openingFloatMinor: number): Promise<void> {
  const response = await api.openShift({
    opening_float: { currency_code: storeCurrency(), amount_minor: openingFloatMinor },
  });
  setState("shift", {
    shiftId: response.shift_id,
    state: response.state,
    expected: response.expected_amount,
    counted: response.counted_amount,
    variance: response.variance,
  });
}

export async function countShift(shiftId: string, countedMinor: number): Promise<void> {
  const response = await api.countShift(shiftId, { counted_minor: countedMinor });
  setState("shift", "state", response.state);
  setState("shift", "counted", response.counted_amount);
}

export async function closeShift(shiftId: string): Promise<ShiftInfo> {
  const response = await api.closeShift(shiftId);
  const info: ShiftInfo = {
    shiftId: response.shift_id,
    state: response.state,
    expected: response.expected_amount,
    counted: response.counted_amount,
    variance: response.variance,
  };
  setState("shift", info);
  return info;
}
