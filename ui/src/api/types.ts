// The wire types the edge speaks that are still mirrored by hand (routes in
// `crates/pos-edge/src/http`). Each moves to `schemas.ts`, an alias of the generated schema, once
// the edge document says exactly what the till receives (ADR-0177); the file then goes.

import type {
  BuyerRequest,
  CounterLine,
  CourseResponse,
  MenuItemResponse,
  ModifierGroup,
  PaymentRequest,
} from "./schemas";

import type { Money, NumberFormat, Quantity } from "../lib/money";

export interface TableResponse {
  table_id: string;
  state: string;
}

// What moving guests to another table did: the order that went with them, and both tables after.
export interface TransferResponse {
  order_id: string;
  from_table: TableResponse;
  to_table: TableResponse;
}

// One line of an open order, from `GET /api/orders/live`.
//
// The fan-out carries what happens *next*, so a device that was not running when a line was added
// has no way to learn it — this read is what a till that reloaded, a tablet that slept and a kitchen
// display switched on mid-service have instead of the events they missed. It carries the line id
// because a screen does not only draw a line, it fires, voids and bumps it.
export interface LiveLine {
  order_line_id: string;
  display_name: string;
  quantity: Quantity;
  // The extended total captured when the line was added — the figure the guest was quoted.
  line_total: Money;
  state: string;
  // Whether a station marked it prepared. Orthogonal to `state`, and what stops a kitchen display
  // re-showing a ticket that was already bumped.
  bumped: boolean;
  // When the line went to the kitchen, RFC 3339. Omitted while it is still on the pad, and by an
  // edge older than the field.
  fired_time?: string;
  // What was chosen for the line (ADR-0127), as ids. Optional on the type because an edge built
  // before the field sends none, and a till upgraded ahead of its store must not read `undefined`
  // as a crash.
  modifier_menu_item_ids?: string[];
  // The seat it was ordered for. Absent from the wire for the table's own lines, and for every line
  // on a store that does not assign seats.
  seat?: number;
  // The course it goes out on (ADR-0130). Absent for a line on none, and for every line on a store
  // that has authored no courses. Optional for `modifier_menu_item_ids`'s reason as well: an edge
  // built before the field sends none.
  course_id?: string;
  // The kitchen station it was fired to. Absent while it is on the pad, and from an edge older than
  // the field.
  station_id?: string;
  // Whether a guest note was written (ADR-0157). With no `note` beside it, the edge lost the text to
  // a restart and the screen says so. Absent from an edge older than the field.
  note_present?: boolean;
  // The note while the edge holds it.
  note?: string;
}

// One open order, table or counter.
export interface LiveOrder {
  order_id: string;
  // Absent for a counter order, which sits on no table (ADR-0093).
  table_id?: string;
  // A bill already open on it: the payment screen settles this one rather than asking for a second,
  // which the edge refuses with a 409.
  bill_id?: string;
  // Every bill still open on it, oldest first: one for an ordinary table, one per unpaid part once
  // its bill has been split (ADR-0128). `bill_id` names only the newest. Absent from an edge older
  // than the field, which never had a way to show a till a split table anyway.
  open_bill_ids?: string[];
  // The channel it was opened on, a `SALES_CHANNEL_*` token: the counter shows a walk-in the book of
  // its channel, and whether the guest eats in (ADR-0160 decision 2). Absent from an edge older than
  // the field, and for an order the edge cannot name a channel for.
  sales_channel?: string;
  lines: LiveLine[];
}

export interface MenuResponse {
  currency: string;
  items: MenuItemResponse[];
  // Whether this store takes tips (§10 `Capability::Tips`). The till shows no tip entry when it is
  // false: the edge refuses a tip on such a store, and offering the guest something that will be
  // refused is worse than not offering it.
  tips_enabled: boolean;
  // Whether this store assigns items to seats (§10 `Capability::Seats`). Off by default — most
  // counters have no seats — and the edge refuses a seat on a store that has it off, so the till
  // offers the picker only when this is true rather than offering an act that would be refused.
  seats_enabled: boolean;
  // What this store *is*. `tables_enabled` decides the home screen; `kds_enabled` decides whether
  // the kitchen board is a destination at all (docs/ui-ux.md §3, §10).
  tables_enabled: boolean;
  kds_enabled: boolean;
  // Every modifier group any item attaches, listed once — an item names the ones it needs by id.
  modifier_groups: ModifierGroup[];
  // Whether this store groups its lines into courses (§10 `Capability::Courses`). The edge refuses
  // a fire-by-course on a store with it off, so the till offers no course control there rather than
  // offering an act that would be refused.
  courses_enabled: boolean;
  // Every course any item goes out on, in service order. Empty until the console authors one.
  courses: CourseResponse[];
  // The payment methods this store accepts, as their wire names, or `null` when nothing is
  // restricted. `null` is not an empty list — it means "no restriction published", so a method added
  // to the enum later keeps working on an unrestricted store.
  accepted_tender: string[] | null;
  // The tip keys the pay screen offers, each a whole percentage of the bill, in the store's order,
  // without a key set to 0 or to a percentage an earlier key offers (ADR-0160 decision 2). Empty when
  // no key is left, and the pay screen then shows no tip row. Absent from an edge older than the
  // setting, and the till then offers the three keys it always did.
  tip_percents?: number[];
  // The most guests the pay screen's even split offers: every number from two up to it (ADR-0160
  // decision 2). Absent from an edge older than the setting, and the till then offers two to six.
  split_ways_max?: number;
  // The channel the counter opens a walk-in on (ADR-0160 decision 2): `WALK_IN_CHANNEL_TAKEAWAY`,
  // `WALK_IN_CHANNEL_DINE_IN`, or `WALK_IN_CHANNEL_ASK`, where the cashier asks each guest. The
  // counter shows the book of that channel. Absent from an edge older than the setting, which opens
  // every walk-in for takeaway, and the counter then shows the takeaway book, as it always did.
  walk_in_channel?: string;
}

// One fee a bill is charged (ADR-0159): which rule, its code, its name in the store's display
// language, what it charged and the tax on it. The edge computes every figure; the till shows them.
export interface FeeLine {
  fee_id: string;
  code: string;
  display_name: string;
  amount: Money;
  tax: Money;
  // Whether the fee's rule lets staff waive it on one bill (ADR-0159 decision 5). Absent from an
  // edge older than the waive, which offers none.
  waivable?: boolean;
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

// The manager who authorised an act that needs a second person (§11.4). Sent with the request and
// never stored: the PIN is typed, used once and dropped, exactly as a sign-in PIN is.
export interface ApproverRequest {
  approver_code: string;
  approver_pin: string;
}

// A void, as the till asks for it. The reason is mandatory; the approval is present only when the
// act needs one, and the edge decides which — an unfired line is an ordinary cancel, a fired one
// and every bill need a manager.
export type VoidRequest = { reason_code_id: string } & Partial<ApproverRequest>;

// A fee waived on one bill, as the till asks for it (ADR-0159 decision 5): the reason, and an
// approver where the person needs one. The edge answers with the bill as it now stands.
export type WaiveFeeRequest = { reason_code_id: string } & Partial<ApproverRequest>;

export type SettleRequest = {
  payments: PaymentRequest[];
  // Absent on every ordinary retail sale, which is nearly every bill.
  buyer?: BuyerRequest;
  // The override approver, where this till's drawer is another person's (ADR-0167 decision 9).
} & Partial<ApproverRequest>;

export interface BillResponse {
  bill_id: string;
  state: string;
  receipt_number?: number;
  total_due?: Money;
  table_state?: string;
  print_receipt: boolean;
  /**
   * What came of the receipt: `PRINTED`, `NO_PRINTER`, `PRINTER_UNAVAILABLE` or `UNPRINTABLE_TEXT`
   * (ADR-0100), plus `QUEUED_TO_AGENT`, `PRINT_AGENT_UNAVAILABLE` and `PRINT_QUEUE_FULL` when the
   * printer's transport belongs to another device (ADR-0112). Absent when the settle asked for no
   * receipt, or from an edge built before C2.
   */
  receipt_print?: string;
  /**
   * What came of opening the cash drawer when the settle took cash (ADR-0165): `OPENED`,
   * `NO_DRAWER` or `DRAWER_UNAVAILABLE`. Absent when no cash was taken, or from an older edge.
   */
  drawer_open?: DrawerOutcome;
}

/**
 * What came of opening the cash drawer (ADR-0165). `NO_DRAWER` is a store with no drawer marked in
 * the console, which is the ordinary case and says nothing; `DRAWER_UNAVAILABLE` is a drawer that
 * should have opened and did not, which the cashier has to hear about.
 */
export type DrawerOutcome = "OPENED" | "NO_DRAWER" | "DRAWER_UNAVAILABLE";

/**
 * The store's money settings, from `GET /api/locale`
 * ([ADR-0105](../../../docs/adr/0105-a-country-pack-is-values.md)).
 *
 * Which notes a guest can hand over is a fact about a country's cash, so it arrives with everything
 * else the cloud publishes rather than from a table compiled into this app.
 */
export interface LocaleResponse {
  currency_code: string;
  /**
   * How many decimal places that currency has: 0 for the đồng and the yen, 2 for the paisa and the
   * cent (ADR-0134). Every amount on the wire is an integer in the minor unit, so this is what says
   * where the decimal point goes — and what the till used to guess from a table with no row for the
   * rupee.
   */
  currency_exponent: number;
  /** Notes a guest hands over, ascending, in minor units. Empty means the exact amount only. */
  cash_denominations: number[];
  /** What the total rounds to in cash, in minor units, or `null` for no rounding. */
  cash_rounding_increment: number | null;
  /** Whether menu prices already contain their tax (ADR-0104). */
  prices_include_tax: boolean;
  /**
   * How this store's country writes a number: the decimal mark, the group mark, and how many digits
   * go in a group (ADR-0136). Sent for the reason `currency_exponent` is — a till cannot work it
   * out, and the one it used to invent was `en-US` for every store in every country.
   */
  number_format: NumberFormat;
  /**
   * The store's display language (a BCP 47 tag), absent when it has set none. A device that has
   * never picked a language starts in this one rather than in English.
   */
  display_language?: string;
}

/**
 * The cloud link and the outbox, from `GET /api/sync` (ADR-0137). What the status bar needs to say
 * "Offline — selling normally" and how many events are waiting. No level refuses a sale.
 */
export interface SyncResponse {
  outbox_depth: number;
  outbox_planned_depth: number;
  outbox_level:
    | "OUTBOX_LEVEL_NORMAL"
    | "OUTBOX_LEVEL_ELEVATED"
    | "OUTBOX_LEVEL_HIGH"
    | "OUTBOX_LEVEL_BEYOND";
  cloud_link: "CLOUD_LINK_UNSPECIFIED" | "CLOUD_LINK_ONLINE" | "CLOUD_LINK_OFFLINE";
  last_sync_time?: string;
  /**
   * What the store server last measured of its own clock against a time server (roadmap-v3 PF4).
   * `UNSPECIFIED` until a measurement succeeds. Optional because an edge older than PF4 sends none
   * of these, and a till must not read that as a clock in step.
   */
  clock_drift?: "CLOCK_DRIFT_UNSPECIFIED" | "CLOCK_DRIFT_OK" | "CLOCK_DRIFT_ALARM";
  /** The offset either way past which the clock alarms, in milliseconds. */
  clock_drift_alarm_ms?: number;
  /** The store server's clock minus the time server's, in milliseconds: positive is fast. */
  clock_offset_ms?: number;
  /** When that was measured, on the store server's clock. */
  clock_measure_time?: string;
}

export interface ShiftResponse {
  shift_id: string;
  /** The till whose drawer it is, where the store keeps a drawer per till (ADR-0167). */
  terminal_device_id?: string;
  /** Whose the drawer is for this session, where the store assigns drawers: an id, never a name. */
  assigned_employee_id?: string;
  state: string;
  expected_amount?: Money;
  counted_amount?: Money;
  variance?: Money;
  print_shift_report: boolean;
  /** What came of printing the shift report, on the close that printed one (ADR-0100 tokens). */
  shift_report_print?: string;
  /**
   * Cash paid into the drawer outside a sale so far, and out of it (ADR-0165). Not the expectation:
   * the cashier entered both. Absent from an edge that predates them.
   */
  paid_in_amount?: Money;
  paid_out_amount?: Money;
  /** What came of opening the drawer, on a paid in or a paid out (ADR-0165). */
  drawer_open?: DrawerOutcome;
  /**
   * Present, and true, while the shift is still open past the business day it opened on
   * (ADR-0167 decision 11), which the Shift screen flags.
   */
  day_ended?: boolean;
}

/** How many drawers the store keeps: its one, or one for each till (ADR-0167 decision 1). */
export type DrawerModel = "DRAWER_MODEL_PER_STORE" | "DRAWER_MODEL_PER_TERMINAL";

/** One drawer the store keeps, from `GET /api/shifts`. */
export interface DrawerEntry {
  /** The till whose drawer it is; absent for the store's one drawer. */
  terminal_device_id?: string;
  /** The till's name; absent for the store's one drawer, and for a till the store no longer lists. */
  name?: string;
  /** What its shift opens on unless somebody types another float. */
  default_float: Money;
  /**
   * The shop's wall clock when its shift opened, `HH:MM` in the store's timezone, which a till's
   * own clock may not keep. Absent while the drawer is closed.
   */
  opened_clock?: string;
  /** Its shift, as blind as `GET /api/shifts/current`; `null` while the drawer is closed. */
  shift: ShiftResponse | null;
}

/** Every drawer the store keeps, and which is this device's own (ADR-0167 decisions 3 and 8). */
export interface DrawersResponse {
  /** The model the store server runs now. */
  drawer_model: DrawerModel;
  /** The model the store has published, while a drawer still open keeps the other. */
  waiting_drawer_model?: DrawerModel;
  /** The till this device is, whose drawer is its own; absent for a device that is no till. */
  terminal_device_id?: string;
  drawers: DrawerEntry[];
  /** Present where each drawer is one person's for its session (ADR-0167 decision 9). */
  drawer_access?: "DRAWER_ACCESS_ASSIGNED";
}

/** Each closed drawer's figures, and what came of the one slip a combined close report prints. */
export interface BatchCloseResponse {
  shifts: ShiftResponse[];
  shift_report_print?: string;
}

/** The opening is recorded; this is whether the drawer sprang. */
export interface OpenDrawerResponse {
  drawer_open: DrawerOutcome;
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

/** One printer the store published, from `GET /api/printers`. */

/**
 * One of the store's tills, from `GET /api/print/agent`
 * ([ADR-0112](../../../docs/adr/0112-print-agents.md)): a `TERMINAL` entry the console created, and
 * who holds it. `held` is `THIS_DEVICE`, `ANOTHER_DEVICE` or `NONE`, and never says which other
 * device.
 */
export interface TerminalEntry {
  agent_device_id: string;
  name: string;
  held: string;
}

/**
 * The store's tills, in the order the store published them, and the till this device is when the
 * store does not list it: absent where this device is one of the listed tills or none. Never another
 * device's.
 */
export interface TerminalsResponse {
  terminals: TerminalEntry[];
  unlisted_agent_device_id?: string;
}

/** What a bind came to: `BOUND`, `HELD_BY_ANOTHER_DEVICE` or `DEVICE_HOLDS_ANOTHER_AGENT`. */
export interface BindResponse {
  outcome: string;
}

/**
 * One of today's settled bills, from `GET /api/bills/settled`: the list the till prints a copy of a
 * receipt from ([ADR-0164](../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)).
 * Newest first, and at most 200 of them.
 */
export interface SettledBill {
  bill_id: string;
  order_id: string;
  /** The table it was paid at. Absent for a counter order, which carries its queue number. */
  table_id?: string;
  /** A counter order's number for the day, when it was given one. */
  queue_number?: number;
  receipt_number: number;
  total_due: Money;
  settle_time: string;
  /**
   * The shop's wall clock when it settled, `HH:MM` in the store's timezone. The edge sends it
   * because a till's own clock may keep another zone.
   */
  settle_clock?: string;
  /** How many copies have been printed. */
  copies: number;
}

/**
 * What the store has taken today, for the Today screen's tile (ADR-0160 decision 2): the bills
 * settled in the current business day, at what their guests paid, and how many. The store's figure
 * and nothing finer, never anyone's own. Served only to a person whose role grants
 * `reports.takings.view`.
 */
export interface TakingsResponse {
  business_date: string;
  takings_amount: Money;
  bill_count: number;
}

