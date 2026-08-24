// The client's small projection: the floor, the order lines on each table, the open bill per table,
// and the shift — folded from the same fan-out events the edge publishes (ADR-0018), so what one
// device does appears on every other. Actions call the typed client and lean on the fan-out to
// reconcile; a line the operator adds shows at once and is de-duplicated when its own event returns.

import { createStore, produce } from "solid-js/store";

import { api } from "../api/client";
import type { LinkStatus, ServerEvent } from "../api/live";
import type { BillResponse, LineRequest, PaymentRequest } from "../api/types";
import { CURRENCY, type MenuItem } from "../lib/menu";
import { money, type Money } from "../lib/money";

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
  tableState: Record<string, string>;
  tableOrder: Record<string, string>;
  orderTable: Record<string, string>;
  lines: Record<string, OrderLine>;
  openBill: Record<string, string>;
  shift: ShiftInfo | null;
}

const tid = (code: string): string => code.padStart(26, "0");

// A fixed eight-table floor until the store's real layout syncs from config (P7).
export const FLOOR: readonly TableCard[] = Array.from({ length: 8 }, (_, index) => {
  const number = index + 1;
  return { id: tid(`T${number.toString().padStart(2, "0")}`), label: String(number), state: "TABLE_STATE_FREE" };
});

const [state, setState] = createStore<StoreShape>({
  link: "connecting",
  tableState: Object.fromEntries(FLOOR.map((table) => [table.id, table.state])),
  tableOrder: {},
  orderTable: {},
  lines: {},
  openBill: {},
  shift: null,
});

export { state };

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

// A fixed kitchen station for the foundation; real stations arrive with the KDS routing config (P7).
const STATION = tid("S01");

// The bill total the operator collects, computed client-side from the captured line totals and the
// bootstrap's single 10% standard rate — the same arithmetic the edge does for one tax class. Tax
// rounds half-up on the whole subtotal, with integer math only so it matches the edge to the đồng.
export function billTotalMinor(tableId: string): {
  subtotal: number;
  tax: number;
  total: number;
} {
  const subtotal = linesForTable(tableId)
    .filter((line) => line.state !== "ORDER_LINE_STATE_VOIDED")
    .reduce((sum, line) => sum + line.lineTotal.amount_minor, 0);
  const tax = Math.floor((subtotal * 1000 + 5000) / 10000);
  return { subtotal, tax, total: subtotal + tax };
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
  return FLOOR.find((table) => table.id === tableId)?.label ?? tableId;
}

export interface KitchenLine {
  orderLineId: string;
  name: string;
  tableLabel: string;
}

// Every fired line still on an open order, newest tables last — what the kitchen and the pass work
// from. A line whose table has been cleaned (its bill settled) drops out, because the order is done.
export function firedLines(): KitchenLine[] {
  const liveOrders = new Set(Object.values(state.tableOrder));
  return Object.values(state.lines)
    .filter((line) => line.state === "ORDER_LINE_STATE_FIRED" && liveOrders.has(line.orderId))
    .map((line) => ({
      orderLineId: line.orderLineId,
      name: line.name,
      tableLabel: tableLabel(state.orderTable[line.orderId] ?? ""),
    }));
}

// Table counts by state, for the Today summary.
export function tableCounts(): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const table of FLOOR) {
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

export async function addItem(tableId: string, item: MenuItem): Promise<void> {
  const line: LineRequest = {
    menu_item_id: item.id,
    display_name: item.name,
    quantity: { milli: 1000 },
    unit_price: money(CURRENCY, item.unitPriceMinor),
    line_total: money(CURRENCY, item.unitPriceMinor),
    tax_class_id: item.taxClassId,
    tax_rate: item.taxRate,
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
        name: item.name,
        quantityMilli: 1000,
        lineTotal: money(CURRENCY, item.unitPriceMinor),
        state: response.state,
      };
    }),
  );
}

export async function fire(lineId: string): Promise<void> {
  const response = await api.fireLine(lineId, { station_id: STATION });
  setState("lines", lineId, "state", response.state);
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
    opening_float: { currency_code: "VND", amount_minor: openingFloatMinor },
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
