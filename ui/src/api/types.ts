// The wire types the edge speaks (P5 routes in `crates/pos-edge/src/http`). Kept as a hand-written
// mirror for the P6 foundation; a generated client from the OpenAPI surface is the P7 follow-up
// (ADR-0019), at which point this file is replaced rather than maintained by hand.

import type { Money, Quantity, Ratio } from "../lib/money";

export interface TableResponse {
  table_id: string;
  state: string;
}

export interface LineRequest {
  menu_item_id: string;
  display_name: string;
  quantity: Quantity;
  unit_price: Money;
  line_total: Money;
  tax_class_id: string;
  tax_rate: Ratio;
  seat?: number;
  course_id?: string;
  note_present: boolean;
}

export interface LineResponse {
  order_id: string;
  order_line_id: string;
  state: string;
}

export interface FireRequest {
  station_id: string;
}

// The store's published floor plan and kitchen stations from `GET /api/floor` (ADR-0072). The edge
// serves the live `EdgeSession` plans; the shapes mirror `pos_proto::floor`, with the optional-field
// keys (`position`, `default_station_id`, …) omitted from the wire when absent.
export interface FloorGridPosition {
  column: number;
  row: number;
}

export interface FloorTable {
  table_id: string;
  label: string;
  seats?: number;
  position?: FloorGridPosition | null;
}

export interface FloorArea {
  area_id: string;
  name: string;
  tables?: FloorTable[];
}

export interface FloorPlan {
  areas?: FloorArea[];
}

export interface KitchenStation {
  station_id: string;
  name: string;
  backup_station_id?: string | null;
}

export interface StationRoutingRule {
  station_id: string;
  menu_item_id?: string | null;
  course_id?: string | null;
}

export interface StationPlan {
  stations?: KitchenStation[];
  routing?: StationRoutingRule[];
  default_station_id?: string | null;
}

export interface FloorResponse {
  floor: FloorPlan;
  stations: StationPlan;
}

// The store's own price book from `GET /api/menu` (roadmap-v3 E5, ADR-0063). Every amount is the
// edge's, already in the store's currency — the app displays it and hands it straight back on a
// line, and never computes one of its own.
export interface MenuItemResponse {
  menu_item_id: string;
  display_name: string;
  unit_price: Money;
  tax_class_id: string;
  // Absent when the store's rate table has no row for this item's class. That is a configuration
  // error, not a zero rate, so the edge also reports the item unavailable.
  tax_rate?: Ratio | null;
  available: boolean;
}

export interface MenuResponse {
  currency: string;
  items: MenuItemResponse[];
}

// What a table owes right now, from `GET /api/tables/{id}/check` (roadmap-v3 E5). Assembled by the
// edge from the order's live lines against the store's own tax table — the same calculation the bill
// settles against, so the till displays a figure rather than deriving one.
export interface CheckResponse {
  subtotal: Money;
  tax_total: Money;
  total_due: Money;
}

export interface BumpRequest {
  order_id: string;
  station_id: string;
  order_line_ids: string[];
}

export interface BumpResponse {
  order_id: string;
  station_id: string;
  order_line_ids: string[];
}

export interface PaymentRequest {
  method: string;
  tendered: Money;
  applied_to_bill: Money;
}

export interface SettleRequest {
  payments: PaymentRequest[];
  tips?: Money[];
}

export interface BillResponse {
  bill_id: string;
  state: string;
  receipt_number?: number;
  total_due?: Money;
  table_state?: string;
  print_receipt: boolean;
}

export interface OpenShiftRequest {
  opening_float: Money;
}

export interface CountShiftRequest {
  counted_minor: number;
}

export interface ShiftResponse {
  shift_id: string;
  state: string;
  expected_amount?: Money;
  counted_amount?: Money;
  variance?: Money;
  print_shift_report: boolean;
}

export interface PairRequest {
  code: string;
}

export interface PairAccepted {
  device_token: string;
}

// The first-boot activation exchange (ADR-0050), mounted only when the store server is provisioned
// for a cloud (ADR-0086) — a LAN-only edge serves neither route.
export interface ActivateRequest {
  code: string;
}

// What a successful activation grants. The device credential itself never reaches the browser: the
// edge stores it in the operating system's keyring and answers with the identity alone.
export interface ActivateAccepted {
  device_id: string;
}

export interface ActivationStanding {
  activated: boolean;
}
