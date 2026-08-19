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
