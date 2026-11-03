// Voiding a paid bill as a money act (ADR-0184, ADR-0196, ADR-0204). The edge gives the cash to hand
// back and the card to reverse; a void the till learned of by reading the day's bills again has no
// such figures, so it is `null` and the screen says to read them from the shift instead.

import { api } from "../api/client";
import type { ApproverRequest, VoidSettledResponse } from "../api/schemas";
import { t } from "../i18n";
import { reasonMessage } from "../lib/errors";
import { PRECHECKED } from "../package/prechecks";
import { type MoneyAct, type Recheck, useMoneyAct } from "./money-act";

const CHECKED = PRECHECKED["billing.bill.void_settled"] ?? [];

/** What the edge says now about a paid bill whose void went unanswered. */
export async function recheckVoidSettled(billId: string): Promise<Recheck<VoidSettledResponse | null>> {
  const bill = (await api.settledBills()).find((entry) => entry.bill_id === billId);
  if (bill === undefined) {
    return null;
  }
  return bill.voided === true ? { committed: null } : { refused: t("today.void_not_made") };
}

export interface VoidSettledAct extends MoneyAct<VoidSettledResponse | null> {
  readonly voidWith: (reasonCodeId: string, asks: boolean, approval: ApproverRequest | undefined) => void;
}

export function useVoidSettledAct(
  billId: string,
  onVoided: (voided: VoidSettledResponse | null) => void,
): VoidSettledAct {
  const act = useMoneyAct<VoidSettledResponse | null>({ recheck: recheckVoidSettled, committed: onVoided });
  const voidWith = (reasonCodeId: string, asks: boolean, approval: ApproverRequest | undefined) =>
    void act.run(
      billId,
      () =>
        CHECKED.includes("APPROVAL_REQUIRED") && asks && approval === undefined
          ? reasonMessage("APPROVAL_REQUIRED")
          : null,
      () => api.voidSettledBill(billId, { reason_code_id: reasonCodeId, ...approval }),
    );
  return { ...act, voidWith };
}
