// Taking payment as a money act (ADR-0196, ADR-0204): the settle's predictable refusals, how the
// till learns what came of a settle the edge did not answer, and the pay screen's hook.

import type { BillResponse, BuyerRequest, CheckPrintedResponse, PaymentRequest } from "../api/schemas";
import { api } from "../api/client";
import { t } from "../i18n";
import { reasonMessage } from "../lib/errors";
import { PRECHECKED, WRITES_PENDING } from "../package/prechecks";
import { type MoneyAct, type Recheck, useMoneyAct } from "./money-act";
import { writesPending } from "./edge-write";
import { applySettled, settle, state } from "./store";

const CHECKED = PRECHECKED["billing.bill.settle"] ?? [];

/**
 * The first refusal the till can decide about this settle, as the sentence the operator reads, or
 * null. `due` is what the edge said the bill comes to, and `printed` the check it said was last
 * printed for the bill, if any (ADR-0218): a store that takes payment only against a fresh check
 * refuses one never printed, or printed before the order last changed.
 */
export function settleRefusal(
  payments: readonly PaymentRequest[],
  due: number,
  printed?: CheckPrintedResponse,
): string | null {
  if (CHECKED.includes(WRITES_PENDING) && writesPending()) {
    return t("pay.wait_for_writes");
  }
  if (CHECKED.includes("CHECK_REQUIRED") && state.checkBeforePayment && (printed === undefined || printed.stale)) {
    return reasonMessage("CHECK_REQUIRED");
  }
  const cash = payments.some((payment) => payment.method === "PAYMENT_METHOD_CASH");
  if (CHECKED.includes("DRAWER_COUNTED") && cash && state.shift?.state === "SHIFT_STATE_COUNTED") {
    return reasonMessage("DRAWER_COUNTED");
  }
  const applied = payments.reduce((sum, payment) => sum + payment.applied_to_bill.amount_minor, 0);
  if (CHECKED.includes("PAYMENTS_DO_NOT_SUM_TO_TOTAL") && applied !== due) {
    return reasonMessage("PAYMENTS_DO_NOT_SUM_TO_TOTAL");
  }
  return null;
}

/**
 * The settled panel's receipt line. A receipt number is a fiscal fact, so a settle the till learned
 * of by reading the bill again, whose number it could not read, says so rather than showing one.
 */
export function receiptLine(bill: BillResponse): string {
  return bill.receipt_number === undefined || bill.receipt_number === null
    ? t("pay.receipt_unread")
    : t("pay.receipt", { number: bill.receipt_number });
}

/**
 * The settled panel's e-invoice line, when the settle issued one (ADR-0242): its number and the code
 * a guest looks it up by. `null` for a deferred invoice, which has no number yet, and for none.
 */
export function invoiceLine(bill: BillResponse): string | null {
  if (bill.invoice_number_pending) {
    // Issued "number pending": the vendor gives the number later, the code works now (ADR-0251).
    return t("pay.einvoice_pending", { code: bill.invoice_lookup_code ?? "" });
  }
  return bill.invoice_number
    ? t("pay.einvoice", { number: bill.invoice_number, code: bill.invoice_lookup_code ?? "" })
    : null;
}

/**
 * Whether a buyer's tax number has a shape some country writes (ADR-0242 decision 4): 10 to 15
 * letters, digits or `-`, which covers a Vietnamese MST (10 digits, or 10 and a 3-digit branch), a
 * Japanese 登録番号 and an Indian GSTIN. Blank is fine: the number is optional. The edge checks the
 * store's own country's shape and refuses the settle if it is wrong.
 */
export function taxCodeLooksValid(code: string): boolean {
  const trimmed = code.trim();
  return trimmed === "" || /^[A-Za-z0-9-]{10,15}$/.test(trimmed);
}

/** What the edge says now about a bill whose settle went unanswered. */
export async function recheckSettle(billId: string): Promise<Recheck<BillResponse>> {
  const bill = await api.billCheck(billId);
  if (bill.state === "BILL_STATE_SETTLED") {
    const settled = (await api.settledBills()).find((entry) => entry.bill_id === billId);
    const response: BillResponse = {
      bill_id: billId,
      state: bill.state,
      print_receipt: false,
      receipt_number: settled?.receipt_number,
    };
    applySettled(billId, response);
    return { committed: response };
  }
  return bill.state === "BILL_STATE_OPEN" ? { refused: t("pay.not_taken") } : { refused: t("pay.ended_unconfirmed") };
}

export interface SettleAct extends MoneyAct<BillResponse> {
  readonly pay: (
    billId: string,
    payments: PaymentRequest[],
    due: number,
    buyer: BuyerRequest | undefined,
    printed?: CheckPrintedResponse,
  ) => Promise<void>;
}

/** The pay screen's settle: `onSettled` gets the edge's answer, or what a re-read found. */
export function useSettleAct(onSettled: (bill: BillResponse) => void): SettleAct {
  const act = useMoneyAct<BillResponse>({ act: "settle", recheck: recheckSettle, committed: onSettled });
  const pay = (
    billId: string,
    payments: PaymentRequest[],
    due: number,
    buyer: BuyerRequest | undefined,
    printed?: CheckPrintedResponse,
  ) =>
    act.run(
      billId,
      () => settleRefusal(payments, due, printed),
      () => settle(billId, payments, buyer),
    );
  return { ...act, pay };
}
