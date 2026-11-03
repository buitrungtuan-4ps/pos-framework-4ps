// The pay screen's two money acts, taking payment and voiding the bill (ADR-0196, ADR-0204), as one
// hook: which is out, what the screen shows meanwhile, and the refusal the cashier must dismiss. Kept
// out of the screen so the screen stays the layout of the pad.

import type { Accessor } from "solid-js";

import type { BillResponse, BuyerRequest, PaymentRequest } from "../api/schemas";
import type { MoneyPhase } from "./money-act";
import { useSettleAct } from "./settle-act";
import { useVoidAct } from "./void-act";

export function usePayActs(options: {
  readonly billId: Accessor<string | null>;
  readonly onSettled: (bill: BillResponse, change: number) => void;
  readonly onVoided: () => void;
}) {
  let paid: PaymentRequest[] = [];
  const settling = useSettleAct((bill) =>
    options.onSettled(
      bill,
      paid.reduce(
        (sum, p) => sum + p.tendered.amount_minor - p.applied_to_bill.amount_minor - (p.tip?.amount_minor ?? 0),
        0,
      ),
    ),
  );
  const voiding = useVoidAct(options.billId, () => options.onVoided());
  const pending = (): { phase: MoneyPhase; act: "settle" | "void" } | null =>
    settling.phase() !== "idle"
      ? { phase: settling.phase(), act: "settle" }
      : voiding.phase() !== "idle"
        ? { phase: voiding.phase(), act: "void" }
        : null;
  const refusal = () => settling.refusal() ?? voiding.refusal();
  return {
    pending,
    refusal,
    dismiss: () => {
      settling.dismiss();
      voiding.dismiss();
    },
    /**
     * Non-null while an act is out (the tender it was sent with, or "void") or its refusal stands
     * ("refused"), so every tender is disabled and only the one sent says it is working.
     */
    busy: (): string | null =>
      pending() !== null ? (paid.at(-1)?.method ?? "void") : refusal() !== null ? "refused" : null,
    resume: () => {
      const id = options.billId();
      if (id !== null) {
        settling.resume(id);
        voiding.resume(id);
      }
    },
    pay: (payments: PaymentRequest[], due: number, buyer: BuyerRequest | undefined) => {
      const id = options.billId();
      if (id !== null) {
        paid = payments;
        void settling.pay(id, payments, due, buyer);
      }
    },
    voidBill: voiding.voidWith,
  };
}
