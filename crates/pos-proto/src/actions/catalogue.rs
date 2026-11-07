// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The entries of the action catalogue, by domain. A new act is a release: its route first, then
//! its entry here, and for money its own ADR (ADR-0191 decision 5).

use super::ActionEffect::{EdgeWrite, Money, Ui};
use super::ActionFrequency::{Common, Rare};
use super::ActionParamKind::{Amount, Count, Entered, Id, IdList, Text, Token};
use super::{Action, optional, required};
use crate::settings::NEXT_RELEASE;

/// What every entry starts from: no parameters, routes, permissions or refusals, introduced in the
/// next release.
const BASE: Action = Action {
    action_id: "",
    effect: Ui,
    frequency: Common,
    params: &[],
    routes: &[],
    permissions: &[],
    refusals: &[],
    since: NEXT_RELEASE,
    summary: "",
};

/// Changing an order's lines.
const ADD_LINE: &[&str] = &["sales.line.add"];
/// Seating, releasing and clearing a table.
const MANAGE_TABLES: &[&str] = &["sales.table.manage"];
/// Opening a bill, and printing the check of what is on it.
const OPEN_BILL: &[&str] = &["billing.bill.open"];
/// Entering custom records and custom field values (ADR-0206 decision 7).
const DATA_RECORD_WRITE: &[&str] = &["data.record.write"];
/// Pairing and terminals.
const MANAGE_DEVICES: &[&str] = &["admin.device.manage"];
/// A cash movement into or out of a drawer, somebody else's with the override.
const CASH_MOVEMENT: &[&str] = &["cash.movement.record", "cash.drawer.override_assignment"];
/// Counting and closing a drawer's shift, another till's too.
const CLOSE_SHIFT: &[&str] = &[
    "cash.shift.close",
    "cash.shift.manage_other_till",
    "cash.drawer.override_assignment",
];
/// What the till decides about a cash movement before it sends.
const CASH_MOVEMENT_REFUSALS: &[&str] = &[
    "SHIFT_NOT_OPEN",
    "CASH_REASON_NOT_VALID",
    "APPROVAL_REQUIRED",
    "DRAWER_ASSIGNED_TO_ANOTHER",
    "NOT_A_TILL",
    "DRAWER_DAY_ENDED",
];

/// Every act, in the order of the domains: the till's own screen, sales, billing, cash, reports,
/// administration.
pub const CATALOGUE: &[Action] = &[
    // ---- The till's own screen ----
    Action {
        action_id: "ui.screen.open",
        params: &[required("screen_id", Id)],
        summary: "Show another screen of the package",
        ..BASE
    },
    Action {
        action_id: "ui.panel.open",
        params: &[required("panel_id", Id)],
        summary: "Open a panel over the screen",
        ..BASE
    },
    Action {
        action_id: "ui.list.filter",
        params: &[required("text", Text)],
        summary: "Narrow a list to what matches the typed text",
        ..BASE
    },
    // ---- Sales ----
    Action {
        action_id: "sales.order.open",
        effect: EdgeWrite,
        params: &[optional("channel", Token)],
        routes: &["POST /api/orders"],
        permissions: ADD_LINE,
        summary: "Open a counter order for a sales channel",
        ..BASE
    },
    Action {
        action_id: "sales.line.add",
        effect: EdgeWrite,
        params: &[
            optional("table_id", Id),
            optional("order_id", Id),
            required("menu_item_id", Id),
            required("quantity", Count),
            optional("modifier_menu_item_ids", IdList),
            optional("course_id", Id),
            optional("seat", Count),
            optional("note", Text),
        ],
        routes: &["POST /api/orders/{id}/lines", "POST /api/tables/{id}/lines"],
        permissions: ADD_LINE,
        summary: "Add an item to the order a table holds, or to a counter order",
        ..BASE
    },
    Action {
        action_id: "sales.line.set_quantity",
        effect: EdgeWrite,
        params: &[required("order_line_id", Id), required("quantity", Count)],
        routes: &["POST /api/lines/{id}/quantity"],
        permissions: ADD_LINE,
        summary: "Change how many of a line",
        ..BASE
    },
    Action {
        action_id: "sales.line.void",
        effect: EdgeWrite,
        frequency: Rare,
        params: &[
            required("order_line_id", Id),
            required("reason_code_id", Id),
        ],
        routes: &["POST /api/lines/{id}/void"],
        permissions: &["sales.line.add", "sales.line.void_fired"],
        refusals: &["VOID_REASON_NOT_VALID", "APPROVAL_REQUIRED"],
        summary: "Take a line off an order, with a reason",
        ..BASE
    },
    Action {
        action_id: "sales.line.fire",
        effect: EdgeWrite,
        frequency: Rare,
        params: &[required("order_line_id", Id), optional("station_id", Id)],
        routes: &["POST /api/lines/{id}/fire"],
        permissions: &["sales.line.fire"],
        summary: "Send one line to the kitchen",
        ..BASE
    },
    Action {
        action_id: "sales.order.fire",
        effect: EdgeWrite,
        params: &[required("order_id", Id), optional("station_id", Id)],
        routes: &["POST /api/orders/{id}/fire"],
        permissions: &["sales.line.fire"],
        summary: "Send every unsent line of an order to the kitchen",
        ..BASE
    },
    Action {
        action_id: "sales.course.fire",
        effect: EdgeWrite,
        params: &[
            required("order_id", Id),
            required("course_id", Id),
            optional("station_id", Id),
        ],
        routes: &["POST /api/orders/{id}/fire/{course_id}"],
        permissions: &["sales.line.fire"],
        summary: "Send one course of an order to the kitchen",
        ..BASE
    },
    Action {
        action_id: "sales.order.confirm",
        effect: EdgeWrite,
        params: &[required("order_id", Id)],
        routes: &["POST /api/orders/{id}/confirm"],
        permissions: &["sales.order.confirm_qr"],
        summary: "Release a guest's QR order to the kitchen",
        ..BASE
    },
    Action {
        action_id: "sales.order.reject",
        effect: EdgeWrite,
        frequency: Rare,
        params: &[required("order_id", Id), required("reason_code_id", Id)],
        routes: &["POST /api/orders/{id}/reject"],
        permissions: &["sales.order.confirm_qr"],
        refusals: &["REASON_CODE_NOT_VALID"],
        summary: "Refuse a guest's QR order, with a reason",
        ..BASE
    },
    Action {
        action_id: "sales.item.mark_sold_out",
        effect: EdgeWrite,
        params: &[required("menu_item_id", Id)],
        routes: &["POST /api/menu/{id}/sold-out"],
        permissions: &["sales.item.mark_unavailable"],
        summary: "Mark an item sold out on every till",
        ..BASE
    },
    Action {
        action_id: "sales.item.restore",
        effect: EdgeWrite,
        params: &[required("menu_item_id", Id)],
        routes: &["POST /api/menu/{id}/restore"],
        permissions: &["sales.item.mark_unavailable"],
        summary: "Bring a sold-out item back",
        ..BASE
    },
    Action {
        action_id: "sales.table.seat",
        effect: EdgeWrite,
        params: &[required("table_id", Id), optional("guest_count", Count)],
        routes: &["POST /api/tables/{id}/seat"],
        permissions: MANAGE_TABLES,
        summary: "Seat a free table and open its order",
        ..BASE
    },
    Action {
        action_id: "sales.table.release",
        effect: EdgeWrite,
        frequency: Rare,
        params: &[required("table_id", Id)],
        routes: &["POST /api/tables/{id}/release"],
        permissions: MANAGE_TABLES,
        refusals: &["ORDER_NOT_EMPTY"],
        summary: "Free a table seated by mistake, while nothing is sold on it",
        ..BASE
    },
    Action {
        action_id: "sales.table.clean",
        effect: EdgeWrite,
        params: &[required("table_id", Id)],
        routes: &["POST /api/tables/{id}/clean"],
        permissions: MANAGE_TABLES,
        summary: "Mark a table cleared and ready to seat",
        ..BASE
    },
    Action {
        action_id: "sales.order.transfer",
        effect: EdgeWrite,
        params: &[required("table_id", Id), required("to_table_id", Id)],
        routes: &["POST /api/tables/{id}/transfer"],
        permissions: &["sales.order.transfer", "sales.table.manage"],
        summary: "Move a table's order and guests to another table",
        ..BASE
    },
    Action {
        action_id: "sales.ticket.bump",
        effect: EdgeWrite,
        params: &[
            required("order_id", Id),
            required("order_line_ids", IdList),
            required("station_id", Id),
        ],
        routes: &["POST /api/kds/bump"],
        permissions: &["sales.ticket.bump"],
        summary: "Mark a kitchen ticket done",
        ..BASE
    },
    // ---- Billing ----
    Action {
        action_id: "billing.bill.open",
        effect: EdgeWrite,
        params: &[optional("table_id", Id), optional("order_id", Id)],
        routes: &["POST /api/orders/{id}/bill", "POST /api/tables/{id}/bill"],
        permissions: OPEN_BILL,
        refusals: &["BILL_ALREADY_OPEN", "NOTHING_TO_BILL"],
        summary: "Open the bill of a table's order or a counter order",
        ..BASE
    },
    Action {
        action_id: "billing.check.print",
        effect: EdgeWrite,
        params: &[
            optional("table_id", Id),
            optional("order_id", Id),
            optional("bill_id", Id),
        ],
        routes: &[
            "POST /api/bills/{id}/check/print",
            "POST /api/orders/{id}/check/print",
            "POST /api/tables/{id}/check/print",
        ],
        permissions: OPEN_BILL,
        summary: "Print the pre-bill: the check so far, unnumbered and not a receipt",
        ..BASE
    },
    Action {
        action_id: "billing.receipt.reprint",
        effect: EdgeWrite,
        frequency: Rare,
        params: &[required("bill_id", Id)],
        routes: &["POST /api/bills/{id}/receipt/reprint"],
        permissions: &["billing.receipt.reprint"],
        refusals: &["NOT_SETTLED"],
        summary: "Print a settled bill's receipt again, marked as a copy",
        ..BASE
    },
    Action {
        action_id: "billing.bill.settle",
        effect: Money,
        frequency: Rare,
        params: &[
            required("bill_id", Id),
            required("payments", Entered),
            optional("buyer", Entered),
        ],
        routes: &["POST /api/bills/{id}/settle"],
        permissions: &["billing.payment.take", "cash.drawer.override_assignment"],
        refusals: &[
            "OPEN_SHIFT_REQUIRED",
            "DRAWER_COUNTED",
            "DRAWER_ASSIGNED_TO_ANOTHER",
            "NOT_A_TILL",
            "PAYMENTS_DO_NOT_SUM_TO_TOTAL",
            "DRAWER_DAY_ENDED",
        ],
        summary: "Take payment for a bill and settle it",
        ..BASE
    },
    Action {
        action_id: "billing.bill.discount",
        effect: Money,
        frequency: Rare,
        params: &[
            required("bill_id", Id),
            required("amount", Amount),
            required("reason_code_id", Id),
        ],
        routes: &["POST /api/bills/{id}/discount"],
        permissions: &[
            "billing.discount.apply",
            "billing.discount.override_ceiling",
        ],
        refusals: &[
            "REASON_CODE_NOT_VALID",
            "APPROVAL_REQUIRED",
            "REDUCTION_EXCEEDS_BILL",
        ],
        summary: "Take an amount off a bill, with a reason",
        ..BASE
    },
    Action {
        action_id: "billing.fee.waive",
        effect: Money,
        frequency: Rare,
        params: &[
            required("bill_id", Id),
            required("fee_id", Id),
            required("reason_code_id", Id),
        ],
        routes: &["POST /api/bills/{id}/fees/{fee_id}/waive"],
        permissions: &["billing.fee.waive"],
        refusals: &[
            "REASON_CODE_NOT_VALID",
            "APPROVAL_REQUIRED",
            "FEE_NOT_WAIVABLE",
        ],
        summary: "Waive a fee on a bill, with a reason",
        ..BASE
    },
    Action {
        action_id: "billing.bill.split",
        effect: Money,
        frequency: Rare,
        params: &[required("bill_id", Id), required("parts", Entered)],
        routes: &["POST /api/bills/{id}/split"],
        permissions: &["billing.bill.split"],
        refusals: &["SPLIT_NOT_A_PARTITION"],
        summary: "Split a bill into parts paid apart",
        ..BASE
    },
    Action {
        action_id: "billing.bill.merge",
        effect: Money,
        frequency: Rare,
        params: &[
            required("bill_id", Id),
            required("absorbed_bill_ids", IdList),
        ],
        routes: &["POST /api/bills/{id}/merge"],
        permissions: &["billing.bill.split"],
        refusals: &["BILLS_ON_DIFFERENT_ORDERS"],
        summary: "Put the parts of a split bill back together",
        ..BASE
    },
    Action {
        action_id: "billing.bill.void",
        effect: Money,
        frequency: Rare,
        params: &[required("bill_id", Id), required("reason_code_id", Id)],
        routes: &["POST /api/bills/{id}/void"],
        permissions: &["billing.bill.void"],
        refusals: &["VOID_REASON_NOT_VALID", "APPROVAL_REQUIRED"],
        summary: "Void an unpaid bill, with a reason",
        ..BASE
    },
    Action {
        action_id: "billing.bill.void_settled",
        effect: Money,
        frequency: Rare,
        params: &[required("bill_id", Id), required("reason_code_id", Id)],
        routes: &["POST /api/bills/{id}/void_settled"],
        permissions: &[
            "billing.bill.void_settled",
            "cash.drawer.override_assignment",
        ],
        refusals: &[
            "VOID_REASON_NOT_VALID",
            "APPROVAL_REQUIRED",
            "SETTLED_VOID_WINDOW_CLOSED",
            "E_INVOICE_ISSUED",
            "NOT_A_TILL",
            "OPEN_SHIFT_REQUIRED",
            "DRAWER_COUNTED",
            "DRAWER_DAY_ENDED",
            "DRAWER_ASSIGNED_TO_ANOTHER",
        ],
        summary: "Void a paid bill whole and give its money back, with a reason",
        ..BASE
    },
    // ---- Cash ----
    Action {
        action_id: "cash.drawer.open",
        effect: Money,
        frequency: Rare,
        params: &[required("reason_code_id", Id)],
        routes: &["POST /api/drawer/open"],
        permissions: &[
            "cash.drawer.open_no_sale",
            "cash.drawer.override_assignment",
        ],
        refusals: &[
            "CASH_REASON_NOT_VALID",
            "NOT_A_TILL",
            "APPROVAL_REQUIRED",
            "DRAWER_ASSIGNED_TO_ANOTHER",
        ],
        summary: "Open the cash drawer with no sale, with a reason",
        ..BASE
    },
    Action {
        action_id: "cash.shift.open",
        effect: Money,
        frequency: Rare,
        params: &[
            required("opening_float", Amount),
            optional("denomination_counts", Entered),
            optional("terminal_device_id", Id),
            optional("assigned_employee_id", Id),
            optional("handed_over_from_shift_id", Id),
        ],
        routes: &["POST /api/shifts"],
        permissions: &["cash.shift.open", "cash.shift.manage_other_till"],
        refusals: &[
            "SHIFT_ALREADY_OPEN",
            "NOT_A_TILL",
            "ASSIGNEE_REQUIRED",
            "COUNT_DOES_NOT_ADD_UP",
            "APPROVAL_REQUIRED",
            "NOT_THE_SHIFT_HANDED_OVER",
        ],
        summary: "Open a drawer's shift with its float",
        ..BASE
    },
    Action {
        action_id: "cash.shift.count",
        effect: Money,
        frequency: Rare,
        params: &[
            required("shift_id", Id),
            required("counted_amount", Amount),
            optional("denomination_counts", Entered),
        ],
        routes: &["POST /api/shifts/{id}/count"],
        permissions: CLOSE_SHIFT,
        refusals: &[
            "COUNT_DOES_NOT_ADD_UP",
            "APPROVAL_REQUIRED",
            "DRAWER_ASSIGNED_TO_ANOTHER",
        ],
        summary: "Record what a drawer holds at the end of its shift",
        ..BASE
    },
    Action {
        action_id: "cash.shift.close",
        effect: Money,
        frequency: Rare,
        params: &[required("shift_id", Id), optional("reason_code_id", Id)],
        routes: &["POST /api/shifts/{id}/close"],
        permissions: CLOSE_SHIFT,
        refusals: &[
            "VARIANCE_REASON_REQUIRED",
            "CASH_REASON_NOT_VALID",
            "APPROVAL_REQUIRED",
            "DRAWER_ASSIGNED_TO_ANOTHER",
        ],
        summary: "Close a counted shift",
        ..BASE
    },
    Action {
        action_id: "cash.shift.close_many",
        effect: Money,
        frequency: Rare,
        params: &[
            required("shift_ids", IdList),
            optional("variance_reasons", Entered),
        ],
        routes: &["POST /api/shifts:batch_close"],
        permissions: CLOSE_SHIFT,
        refusals: &[
            "VARIANCE_REASON_REQUIRED",
            "APPROVAL_REQUIRED",
            "DRAWER_ASSIGNED_TO_ANOTHER",
        ],
        summary: "Close several counted shifts at the end of the day",
        ..BASE
    },
    Action {
        action_id: "cash.movement.paid_in",
        effect: Money,
        frequency: Rare,
        params: &[
            required("shift_id", Id),
            required("amount", Amount),
            required("reason_code_id", Id),
        ],
        routes: &["POST /api/shifts/{id}/paid-in"],
        permissions: CASH_MOVEMENT,
        refusals: CASH_MOVEMENT_REFUSALS,
        summary: "Put cash into a drawer, with a reason",
        ..BASE
    },
    Action {
        action_id: "cash.movement.paid_out",
        effect: Money,
        frequency: Rare,
        params: &[
            required("shift_id", Id),
            required("amount", Amount),
            required("reason_code_id", Id),
        ],
        routes: &["POST /api/shifts/{id}/paid-out"],
        permissions: CASH_MOVEMENT,
        refusals: CASH_MOVEMENT_REFUSALS,
        summary: "Take cash out of a drawer, with a reason",
        ..BASE
    },
    Action {
        action_id: "cash.movement.safe_drop",
        effect: Money,
        frequency: Rare,
        params: &[
            required("shift_id", Id),
            required("amount", Amount),
            optional("denomination_counts", Entered),
            optional("reason_code_id", Id),
        ],
        routes: &["POST /api/shifts/{id}/safe-drop"],
        permissions: CASH_MOVEMENT,
        refusals: &[
            "SHIFT_NOT_OPEN",
            "COUNT_DOES_NOT_ADD_UP",
            "CASH_REASON_NOT_VALID",
            "APPROVAL_REQUIRED",
            "DRAWER_ASSIGNED_TO_ANOTHER",
            "NOT_A_TILL",
            "DRAWER_DAY_ENDED",
        ],
        summary: "Move cash from a drawer to the safe",
        ..BASE
    },
    // ---- Reports ----
    Action {
        action_id: "reports.shift.print_x",
        effect: EdgeWrite,
        frequency: Rare,
        params: &[optional("shift_id", Id)],
        routes: &["POST /api/shifts:print_x"],
        permissions: &["reports.takings.view", "reports.shift.read_x"],
        summary: "Print a shift's X report, or the store's",
        ..BASE
    },
    Action {
        action_id: "reports.shift.print_handover",
        effect: EdgeWrite,
        frequency: Rare,
        params: &[required("shift_id", Id)],
        routes: &["POST /api/shifts/{id}/handover-slip/print"],
        permissions: &["reports.shift.read_x"],
        summary: "Print the slip a drawer is handed over with",
        ..BASE
    },
    // ---- Administration ----
    Action {
        action_id: "admin.pairing_code.create",
        effect: EdgeWrite,
        frequency: Rare,
        routes: &["POST /api/pair/codes"],
        permissions: MANAGE_DEVICES,
        summary: "Mint a code that pairs a new device",
        ..BASE
    },
    Action {
        action_id: "admin.device.revoke",
        effect: EdgeWrite,
        frequency: Rare,
        params: &[optional("device_id", Id)],
        routes: &["POST /api/pair/revoke"],
        permissions: MANAGE_DEVICES,
        summary: "Take a paired device's credential back",
        ..BASE
    },
    Action {
        action_id: "admin.print_agent.claim",
        effect: EdgeWrite,
        frequency: Rare,
        params: &[required("agent_device_id", Id)],
        routes: &["POST /api/print/agent"],
        permissions: MANAGE_DEVICES,
        summary: "Bind a terminal as the print agent of this device",
        ..BASE
    },
    Action {
        action_id: "admin.print_agent.revoke",
        effect: EdgeWrite,
        frequency: Rare,
        params: &[required("agent_device_id", Id)],
        routes: &["POST /api/print/agent/revoke"],
        permissions: MANAGE_DEVICES,
        summary: "Release a terminal bound as a print agent",
        ..BASE
    },
    Action {
        action_id: "admin.printer.test",
        effect: EdgeWrite,
        frequency: Rare,
        params: &[required("printer_id", Id)],
        routes: &["POST /api/printers/{id}/test"],
        permissions: MANAGE_DEVICES,
        summary: "Print a test page on a printer",
        ..BASE
    },
    // ---- Custom data (ADR-0194, ADR-0206) ----
    // Each takes its values as a person enters them on the act's own screen, so a package never
    // binds them and a reaction cannot fill in a custom value, personal or not.
    Action {
        action_id: "data.custom_field.set",
        effect: EdgeWrite,
        frequency: Rare,
        params: &[
            required("object", Token),
            required("object_id", Id),
            required("values", Entered),
        ],
        routes: &["POST /api/custom-fields"],
        permissions: &[
            "data.record.write",
            "sales.line.add",
            "billing.bill.open",
            "sales.table.manage",
            "cash.shift.open",
        ],
        refusals: &["UNKNOWN_OBJECT", "CUSTOM_ENTRY_REFUSED"],
        summary: "Set custom fields on an order, line, bill, table or shift",
        ..BASE
    },
    Action {
        action_id: "data.custom_record.create",
        effect: EdgeWrite,
        frequency: Rare,
        params: &[required("object_type_id", Id), required("values", Entered)],
        routes: &["POST /api/custom-records"],
        permissions: DATA_RECORD_WRITE,
        refusals: &["CUSTOM_ENTRY_REFUSED"],
        summary: "Create a record of a package's object type",
        ..BASE
    },
    Action {
        action_id: "data.custom_record.change",
        effect: EdgeWrite,
        frequency: Rare,
        params: &[
            required("custom_record_id", Id),
            required("object_type_id", Id),
            required("values", Entered),
        ],
        routes: &["POST /api/custom-records/{id}/values"],
        permissions: DATA_RECORD_WRITE,
        refusals: &["UNKNOWN_CUSTOM_RECORD", "CUSTOM_ENTRY_REFUSED"],
        summary: "Change a custom record's values",
        ..BASE
    },
    Action {
        action_id: "data.custom_record.print",
        effect: EdgeWrite,
        frequency: Rare,
        params: &[required("custom_record_id", Id)],
        routes: &["POST /api/custom-records/{id}/slip"],
        permissions: DATA_RECORD_WRITE,
        refusals: &["UNKNOWN_CUSTOM_RECORD", "NOTHING_TO_PRINT"],
        summary: "Print a custom record's slip",
        ..BASE
    },
];
