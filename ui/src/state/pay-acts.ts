// The pay screen's money acts, taking payment (whole, or one share at a time: ADR-0220) and voiding
// the bill (ADR-0196, ADR-0204), as one
// hook: which is out, what the screen shows meanwhile, and the refusal the cashier must dismiss. Kept
// out of the screen so the screen stays the layout of the pad.

import type { Accessor } from "solid-js";

import type {
  BillResponse,
  BuyerRequest,
  CheckPrintedResponse,
  PaymentRequest,
  TakePaymentResponse,
  VoidBillResponse,
} from "../api/schemas";
import type { MoneyPhase } from "./money-act";
import { useSettleAct } from "./settle-act";
import { applySettled } from "./store";
import { type Share, useTakeAct } from "./take-act";
import { useVoidAct } from "./void-act";

export function usePayActs(options: {
  readonly billId: Accessor<string | null>;
  readonly onSettled: (bill: BillResponse, change: number) => void;
  readonly onVoided: (voided: VoidBillResponse) => void;
  /** A share taken that left the bill open, with the change it was owed. */
  readonly onTaken: (answer: TakePaymentResponse, change: number) => void;
}) {
  let paid: PaymentRequest[] = [];
  const change = () =>
    paid.reduce(
      (sum, p) => sum + p.tendered.amount_minor - p.applied_to_bill.amount_minor - (p.tip?.amount_minor ?? 0),
      0,
    );
  const settling = useSettleAct((bill) => options.onSettled(bill, change()));
  // A share that turned out to cover what remained settled the bill, as a settle does.
  const taking = useTakeAct((answer) => {
    if (answer.settled !== undefined) {
      applySettled(answer.bill_id, answer.settled);
      options.onSettled(answer.settled, change());
    } else {
      options.onTaken(answer, change());
    }
  });
  const voiding = useVoidAct(options.billId, (voided) => options.onVoided(voided));
  const pending = (): { phase: MoneyPhase; act: "settle" | "take" | "void" } | null =>
    settling.phase() !== "idle"
      ? { phase: settling.phase(), act: "settle" }
      : taking.phase() !== "idle"
        ? { phase: taking.phase(), act: "take" }
        : voiding.phase() !== "idle"
          ? { phase: voiding.phase(), act: "void" }
          : null;
  const refusal = () => settling.refusal() ?? taking.refusal() ?? voiding.refusal();
  return {
    pending,
    refusal,
    dismiss: () => {
      settling.dismiss();
      taking.dismiss();
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
        taking.resume(id);
        voiding.resume(id);
      }
    },
    pay: (
      payments: PaymentRequest[],
      due: number,
      buyer: BuyerRequest | undefined,
      printed?: CheckPrintedResponse,
    ) => {
      const id = options.billId();
      if (id !== null) {
        paid = payments;
        void settling.pay(id, payments, due, buyer, printed);
      }
    },
    /** Takes one share of an even split, leaving the bill open. */
    take: (share: Share, printed?: CheckPrintedResponse) => {
      const id = options.billId();
      if (id !== null) {
        paid = [share.payment];
        void taking.take(id, share, printed);
      }
    },
    voidBill: voiding.voidWith,
  };
}
