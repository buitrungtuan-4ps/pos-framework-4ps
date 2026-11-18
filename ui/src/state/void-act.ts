// Voiding an unpaid bill as a money act (ADR-0196, ADR-0204): its predictable refusals, and how the
// till learns what came of a void the edge did not answer.

import { api } from "../api/client";
import type { ApproverRequest, VoidBillResponse } from "../api/schemas";
import { t } from "../i18n";
import { reasonMessage } from "../lib/errors";
import { PRECHECKED, WRITES_PENDING } from "../package/prechecks";
import type { Accessor } from "solid-js";

import { type MoneyAct, type Recheck, useMoneyAct } from "./money-act";
import { writesPending } from "./edge-write";
import { applyVoided, voidBill } from "./store";

const CHECKED = PRECHECKED["billing.bill.void"] ?? [];

/**
 * The first refusal the till can decide about this void, or null: a write still on its way, or a
 * manager's approval the store asks for and nobody has typed.
 */
export function voidRefusal(asksApprover: boolean, approval: ApproverRequest | undefined): string | null {
  if (CHECKED.includes(WRITES_PENDING) && writesPending()) {
    return t("pay.wait_for_writes");
  }
  if (CHECKED.includes("APPROVAL_REQUIRED") && asksApprover && approval === undefined) {
    return reasonMessage("APPROVAL_REQUIRED");
  }
  return null;
}

/** What the edge says now about a bill whose void went unanswered. */
export async function recheckVoid(billId: string): Promise<Recheck<VoidBillResponse>> {
  const bill = await api.billCheck(billId);
  if (bill.state === "BILL_STATE_VOIDED") {
    applyVoided(billId);
    // What was given back is not on the read; the screen says the void, not the amounts.
    return { committed: { state: bill.state } };
  }
  return bill.state === "BILL_STATE_OPEN" ? { refused: t("pay.void_not_made") } : { refused: t("pay.ended_unconfirmed") };
}

export interface VoidAct extends MoneyAct<VoidBillResponse> {
  /** Sends the void of the bill on screen, unless the till can tell it would be refused. */
  readonly voidWith: (reasonCodeId: string, asks: boolean, approval: ApproverRequest | undefined) => void;
}

/**
 * A screen's bill void as a money act: `onVoided` runs once the edge commits it, with what it gave
 * back of a part-paid bill's payments (ADR-0220 decision 4).
 */
export function useVoidAct(billId: Accessor<string | null>, onVoided: (voided: VoidBillResponse) => void): VoidAct {
  const act = useMoneyAct<VoidBillResponse>({ act: "void", recheck: recheckVoid, committed: onVoided });
  const voidWith = (reasonCodeId: string, asks: boolean, approval: ApproverRequest | undefined) => {
    const id = billId();
    if (id !== null) {
      void act.run(
        id,
        () => voidRefusal(asks, approval),
        () => voidBill(id, reasonCodeId, approval),
      );
    }
  };
  return { ...act, voidWith };
}
