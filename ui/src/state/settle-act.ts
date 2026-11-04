// Taking payment as a money act (ADR-0196, ADR-0204): the settle's predictable refusals, how the
// till learns what came of a settle the edge did not answer, and the pay screen's hook.

import type { BillResponse, BuyerRequest, PaymentRequest } from "../api/schemas";
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
 * null. `due` is what the edge said the bill comes to.
 */
export function settleRefusal(payments: readonly PaymentRequest[], due: number): string | null {
  if (CHECKED.includes(WRITES_PENDING) && writesPending()) {
    return t("pay.wait_for_writes");
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
  ) => Promise<void>;
}

/** The pay screen's settle: `onSettled` gets the edge's answer, or what a re-read found. */
export function useSettleAct(onSettled: (bill: BillResponse) => void): SettleAct {
  const act = useMoneyAct<BillResponse>({ act: "settle", recheck: recheckSettle, committed: onSettled });
  const pay = (billId: string, payments: PaymentRequest[], due: number, buyer: BuyerRequest | undefined) =>
    act.run(
      billId,
      () => settleRefusal(payments, due),
      () => settle(billId, payments, buyer),
    );
  return { ...act, pay };
}
