// Taking money off a bill as money acts (ADR-0196, ADR-0204): a discount and a fee's waive. Each is
// refused at once where the till can tell, pending until the edge answers, and, with no answer,
// checked by reading the bill again: a discount by whether the bill's discount grew past what it
// was just before the request, a waive by whether the fee has left the bill.

import { type ApiError, api } from "../api/client";
import type { ApproverRequest, CheckResponse } from "../api/schemas";
import { t } from "../i18n";
import { reasonMessage } from "../lib/errors";
import type { Money } from "../lib/money";
import { PRECHECKED, WRITES_PENDING } from "../package/prechecks";
import { type MoneyAct, type Recheck, useMoneyAct } from "./money-act";
import { writesPending } from "./optimistic";
import { applyDiscount, waiveFee } from "./store";

/** The first of an act's checks that refuses, as the operator's sentence, or null. */
function firstRefusal(checks: readonly (readonly [string, boolean])[], checked: readonly string[]): string | null {
  for (const [token, refused] of checks) {
    if (refused && checked.includes(token)) {
      return token === WRITES_PENDING ? t("pay.wait_for_writes") : reasonMessage(token);
    }
  }
  return null;
}

/** What the till can tell about a discount before sending it. */
export function discountRefusal(options: {
  readonly amount: number;
  readonly due: number | null;
  readonly needsApprover: boolean;
  readonly approval: ApproverRequest | undefined;
}): string | null {
  return firstRefusal(
    [
      [WRITES_PENDING, writesPending()],
      ["APPROVAL_REQUIRED", options.needsApprover && options.approval === undefined],
      ["REDUCTION_EXCEEDS_BILL", options.due !== null && options.amount > options.due],
    ],
    PRECHECKED["billing.bill.discount"] ?? [],
  );
}

/** What the till can tell about a waive before sending it. */
export function waiveRefusal(options: {
  readonly waivable: boolean;
  readonly asks: boolean;
  readonly approval: ApproverRequest | undefined;
}): string | null {
  return firstRefusal(
    [
      [WRITES_PENDING, writesPending()],
      ["APPROVAL_REQUIRED", options.asks && options.approval === undefined],
      ["FEE_NOT_WAIVABLE", !options.waivable],
    ],
    PRECHECKED["billing.fee.waive"] ?? [],
  );
}

export interface DiscountAct extends MoneyAct<CheckResponse> {
  readonly send: (
    billId: string,
    amount: Money,
    reasonCodeId: string,
    approval: ApproverRequest | undefined,
    precheck: () => string | null,
  ) => void;
}

/** A screen's discount as a money act; `onApplied` takes the bill as the edge re-priced it. */
export function useDiscountAct(
  onApplied: (totals: CheckResponse) => void,
  refusalMessage?: (caught: ApiError) => string,
): DiscountAct {
  // The bill's discount just before the request; null when that read never came back, which means
  // the request was never sent.
  let before: number | null = null;
  const recheck = async (billId: string): Promise<Recheck<CheckResponse>> => {
    const bill = await api.billCheck(billId);
    if (bill.state !== "BILL_STATE_OPEN") {
      return { refused: t("pay.ended_unconfirmed") };
    }
    return before !== null && bill.discount_total.amount_minor > before
      ? { committed: bill }
      : { refused: t("pay.discount_not_made") };
  };
  const act = useMoneyAct<CheckResponse>({ recheck, committed: onApplied, refusalMessage });
  const send: DiscountAct["send"] = (billId, amount, reasonCodeId, approval, precheck) => {
    before = null;
    void act.run(billId, precheck, async () => {
      before = (await api.billCheck(billId)).discount_total.amount_minor;
      return applyDiscount(billId, amount, reasonCodeId, approval);
    });
  };
  return { ...act, send };
}

export interface WaiveAct extends MoneyAct<CheckResponse> {
  readonly send: (
    billId: string,
    feeId: string,
    reasonCodeId: string,
    approval: ApproverRequest | undefined,
    precheck: () => string | null,
  ) => void;
}

/** A screen's fee waive as a money act; `onWaived` takes the bill as the edge now has it. */
export function useWaiveAct(onWaived: (totals: CheckResponse) => void): WaiveAct {
  let feeId: string | null = null;
  const recheck = async (billId: string): Promise<Recheck<CheckResponse>> => {
    const bill = await api.billCheck(billId);
    if (bill.state !== "BILL_STATE_OPEN") {
      return { refused: t("pay.ended_unconfirmed") };
    }
    return bill.fee_lines.some((line) => line.fee_id === feeId)
      ? { refused: t("pay.waive_not_made") }
      : { committed: bill };
  };
  const act = useMoneyAct<CheckResponse>({ recheck, committed: onWaived });
  const send: WaiveAct["send"] = (billId, fee, reasonCodeId, approval, precheck) => {
    feeId = fee;
    void act.run(billId, precheck, () => waiveFee(billId, fee, reasonCodeId, approval));
  };
  return { ...act, send };
}
