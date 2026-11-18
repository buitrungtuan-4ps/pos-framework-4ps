// Taking one payment towards a bill as a money act (ADR-0220, ADR-0196, ADR-0204): an even split's
// shares before the last. The edge records each as it is taken, so the bill's read, on every till,
// says what is paid and what remains; the last share goes through the settle, which counts them.

import { api } from "../api/client";
import type { CheckPrintedResponse, PaymentRequest, TakePaymentResponse } from "../api/schemas";
import { t } from "../i18n";
import { reasonMessage } from "../lib/errors";
import { PRECHECKED, WRITES_PENDING } from "../package/prechecks";
import { ownOverride } from "./drawer-access";
import { writesPending } from "./edge-write";
import { type MoneyAct, type Recheck, useMoneyAct } from "./money-act";
import { state } from "./store";

const CHECKED = PRECHECKED["billing.payment.take"] ?? [];

/**
 * The first refusal the till can decide about taking `payment` towards a bill that still owes
 * `remaining`, as the sentence the operator reads, or null.
 */
export function takeRefusal(
  payment: PaymentRequest,
  remaining: number,
  printed?: CheckPrintedResponse,
): string | null {
  if (CHECKED.includes(WRITES_PENDING) && writesPending()) {
    return t("pay.wait_for_writes");
  }
  if (CHECKED.includes("CHECK_REQUIRED") && state.checkBeforePayment && (printed === undefined || printed.stale)) {
    return reasonMessage("CHECK_REQUIRED");
  }
  const cash = payment.method === "PAYMENT_METHOD_CASH";
  if (CHECKED.includes("DRAWER_COUNTED") && cash && state.shift?.state === "SHIFT_STATE_COUNTED") {
    return reasonMessage("DRAWER_COUNTED");
  }
  if (CHECKED.includes("PAYMENT_EXCEEDS_REMAINING") && payment.applied_to_bill.amount_minor > remaining) {
    return reasonMessage("PAYMENT_EXCEEDS_REMAINING");
  }
  return null;
}

/**
 * What the edge says now about a payment that went unanswered, given how many the bill had taken
 * when it was sent: one more is the payment taken, and none more is a payment never taken.
 */
export async function recheckTake(billId: string, before: number): Promise<Recheck<TakePaymentResponse>> {
  const bill = await api.billCheck(billId);
  const taken = bill.payments ?? [];
  if (bill.state === "BILL_STATE_OPEN" && taken.length > before && bill.paid_total && bill.remaining) {
    return { committed: { bill_id: billId, paid_total: bill.paid_total, remaining: bill.remaining } };
  }
  return bill.state === "BILL_STATE_OPEN" ? { refused: t("pay.not_taken") } : { refused: t("pay.ended_unconfirmed") };
}

/** One share of an even split of `ways`, on a bill still owing `remaining` with `before` taken. */
export interface Share {
  readonly payment: PaymentRequest;
  readonly ways: number;
  readonly remaining: number;
  readonly before: number;
}

export interface TakeAct extends MoneyAct<TakePaymentResponse> {
  readonly take: (billId: string, share: Share, printed?: CheckPrintedResponse) => Promise<void>;
}

/** The pay screen's share of an even split: `onTaken` gets the edge's answer, or what a re-read found. */
export function useTakeAct(onTaken: (answer: TakePaymentResponse) => void): TakeAct {
  let sentWith = 0;
  const act = useMoneyAct<TakePaymentResponse>({
    act: "take",
    recheck: (billId) => recheckTake(billId, sentWith),
    committed: onTaken,
  });
  const take = (billId: string, share: Share, printed?: CheckPrintedResponse) => {
    sentWith = share.before;
    return act.run(
      billId,
      () => takeRefusal(share.payment, share.remaining, printed),
      () => api.takePayment(billId, { payment: share.payment, split_ways: share.ways, ...ownOverride() }),
    );
  };
  return { ...act, take };
}
