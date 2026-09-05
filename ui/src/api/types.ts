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
  // Whether this store takes tips (§10 `Capability::Tips`). The till shows no tip entry when it is
  // false: the edge refuses a tip on such a store, and offering the guest something that will be
  // refused is worse than not offering it.
  tips_enabled: boolean;
  // The payment methods this store accepts, as their wire names, or `null` when nothing is
  // restricted. `null` is not an empty list — it means "no restriction published", so a method added
  // to the enum later keeps working on an unrestricted store.
  accepted_tender: string[] | null;
}

// What a table owes right now, from `GET /api/tables/{id}/check` (roadmap-v3 E5). Assembled by the
// edge from the order's live lines against the store's own tax table — the same calculation the bill
// settles against, so the till displays a figure rather than deriving one.
export interface CheckResponse {
  subtotal: Money;
  tax_total: Money;
  total_due: Money;
}

// One line of a counter order, as the counter list shows it: what it is and how many, so a cashier
// recognises the order a customer is collecting.
export interface CounterLine {
  display_name: string;
  quantity: Quantity;
}

// A counter order awaiting payment (ADR-0093). A relayed or QR-counter order sits on no table, so it
// appears on no floor plan — this list is the only way a cashier can find it.
export interface CounterOrder {
  order_id: string;
  // The daily number staff shouted. Absent for an order that was never given one; the edge reads
  // the number rather than minting one, so a screen refresh never invents a number.
  queue_number?: number;
  items: CounterLine[];
  total_due: Money;
  // A bill already open on this order. The screen settles THIS bill rather than opening a second
  // one, which the edge refuses with a 409.
  bill_id?: string;
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
  // The tip taken on this tender, held apart from the sale and never part of the bill total. On the
  // payment rather than beside it (roadmap B1.3): tips used to be a separate `tips` list on the
  // settle request with no correspondence to the payments, so no captured payment could record its
  // own tip and each one's change was over-reported by exactly the tip. Optional — omit it and the
  // tender carries no tip, which is how every device behaved before the field existed.
  tip?: Money;
}

/**
 * The corporate customer a tax invoice is issued to
 * ([ADR-0107](../../../docs/adr/0107-the-buyer-is-a-subject.md)).
 *
 * Personal data, all of it. The edge files it in the store's subject store and the settled event
 * carries only a subject id, so erasing a buyer is scrubbing one row and the day's takings are
 * unchanged. Never logged by this app and never held after the settle returns.
 */
export interface BuyerRequest {
  name: string;
  // Format-checked by the compiled-in country module (a 登録番号, a GSTIN, an MST) and never checked
  // for existence — that is a call to the tax authority, and a cashier has to be able to take a
  // corporate customer's number with the line down.
  tax_code?: string;
  address?: string;
  email?: string;
}

export interface SettleRequest {
  payments: PaymentRequest[];
  // Absent on every ordinary retail sale, which is nearly every bill.
  buyer?: BuyerRequest;
}

export interface BillResponse {
  bill_id: string;
  state: string;
  receipt_number?: number;
  total_due?: Money;
  table_state?: string;
  print_receipt: boolean;
  /**
   * What came of the receipt: `PRINTED`, `NO_PRINTER`, `PRINTER_UNAVAILABLE` or `UNPRINTABLE_TEXT`
   * (ADR-0100). Absent when the settle asked for no receipt, or from an edge built before C2.
   */
  receipt_print?: string;
}

/**
 * The store's money settings, from `GET /api/locale`
 * ([ADR-0105](../../../docs/adr/0105-a-country-pack-is-values.md)).
 *
 * Which notes a guest can hand over is a fact about a country's cash, so it arrives with everything
 * else the cloud publishes rather than from a table compiled into this app.
 */
export interface LocaleResponse {
  currency_code: string;
  /** Notes a guest hands over, ascending, in minor units. Empty means the exact amount only. */
  cash_denominations: number[];
  /** What the total rounds to in cash, in minor units, or `null` for no rounding. */
  cash_rounding_increment: number | null;
  /** Whether menu prices already contain their tax (ADR-0104). */
  prices_include_tax: boolean;
}

/** One item's button on the till, from `GET /api/layout` (ADR-0066, C4). */
export interface LayoutButton {
  /** The item this orders — the id the price book from `GET /api/menu` carries. */
  menu_item_id: string;
  /** The caption the console wrote, which may be shorter than the item's catalog name. */
  label: string;
  /** Zero-based grid column, absent for a flowing layout where order alone places the button. */
  column?: number;
  /** Zero-based grid row, absent for the same reason. */
  row?: number;
}

/** A second grouping level under a category. */
export interface LayoutSubcategory {
  display_subcategory_id: string;
  name: string;
  buttons: LayoutButton[];
}

/** A display category: a tab or section on the till. */
export interface LayoutCategory {
  display_category_id: string;
  name: string;
  buttons: LayoutButton[];
  subcategories: LayoutSubcategory[];
}

/**
 * The store's presentation plan for this channel. An empty `categories` means the console has laid
 * nothing out, and the till draws the flat price book instead.
 */
export interface LayoutResponse {
  categories: LayoutCategory[];
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

// One device this store has admitted, from `GET /api/pair/devices` (ADR-0091,
// production-readiness O1). The edge does not know a device's *name* — that lives in the cloud's
// approved-device registry, and a store that has never synced has none — so the pairing instant and
// the `this_device` mark are what let an operator tell the tills apart.
export interface PairedDevice {
  readonly device_id: string;
  readonly paired_at_ms: number;
  /** Whether this is the tablet making the request. Retiring it signs this browser out. */
  readonly this_device: boolean;
}

// What the store says about its own pairing: how many devices, whether that survives a restart, and
// which they are.
export interface PairingState {
  readonly devices: number;
  readonly durable: boolean;
  readonly paired: readonly PairedDevice[];
}

// Which device to retire. An absent `device_id` retires every one of them — the break-glass.
export interface RevokeRequest {
  device_id?: string;
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
