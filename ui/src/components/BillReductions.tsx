// Taking money off a bill and voiding it, for the counter's pay pad (UX-10): the table pay screen's
// own two panels (`./DiscountPanel`, `./VoidBillPanel`), each behind its button, under one manager's
// badge and PIN.
//
// The table pay screen draws the two buttons itself, because the step gate resolves a declared tap
// in the screen's own file (ADR-0109). The counter declares no tap for either, so its pad takes
// both, buttons and panels, from here. A split stays the table's: a walk-in is one guest's bill.

import { Show, type Accessor } from "solid-js";

import type { CheckResponse } from "../api/schemas";
import { t } from "../i18n";
import { can } from "../state/permissions";
import { reasonsFor } from "../state/store";
import { useVoidAct } from "../state/void-act";
import { PendingSettle } from "./PendingSettle";
import { RefusalPanel } from "./RefusalPanel";
import { createApprover } from "./ApproverFields";
import { DISCOUNT, DiscountPanel, useDiscount } from "./DiscountPanel";
import { WhyUnavailable } from "./StoreData";
import { VOID_BILL, VoidBillPanel, useVoidBill } from "./VoidBillPanel";

export function BillReductions(props: {
  billId: Accessor<string | null>;
  currency: Accessor<string>;
  /** The screen's error line. */
  report: (message: string | null) => void;
  /** The bill as the edge re-priced it after a discount. */
  onApplied: (totals: CheckResponse) => void;
  onVoided: () => void;
}) {
  const approver = createApprover();
  // The void as a money act (ADR-0196, ADR-0204): pending until the edge answers, never re-sent.
  const voidAct = useVoidAct(props.billId, () => {
    voiding.closeVoid();
    props.onVoided();
  });
  const discount = useDiscount({
    billId: props.billId,
    currency: props.currency,
    approver,
    report: props.report,
    onApplied: (totals) => props.onApplied(totals),
  });
  const voiding = useVoidBill({
    billId: props.billId,
    approver,
    report: props.report,
    voidWith: voidAct.voidWith,
  });
  // One panel at a time, as on the table: opening one puts the other away.
  const askDiscount = () => {
    voiding.closeVoid();
    discount.askDiscount();
  };
  const askVoid = () => {
    discount.closeDiscount();
    voiding.askVoidBill();
  };
  const button =
    "mt-6 min-h-touch w-full rounded-token border border-line text-sm text-ink-muted disabled:opacity-50";

  return (
    <>
      <Show
        when={discount.open()}
        fallback={
          <Show when={can("billing.discount.apply")}>
            <button
              type="button"
              class={button}
              disabled={props.billId() === null || reasonsFor(DISCOUNT).length === 0}
              aria-describedby="why-counter-reasons"
              data-outcome="counter-discount"
              onClick={askDiscount}
            >
              {t("pay.discount")}
            </button>
          </Show>
        }
      >
        <DiscountPanel state={discount} approver={approver} data-step="discountReason" onSubmit={(id) => discount.discountReason(id)} />
      </Show>
      <Show
        when={voiding.open()}
        fallback={
          <Show when={can("billing.bill.void")}>
            <button
              type="button"
              class={button}
              disabled={props.billId() === null || reasonsFor(VOID_BILL).length === 0}
              aria-describedby="why-counter-reasons"
              data-outcome="counter-void"
              onClick={askVoid}
            >
              {t("pay.void")}
            </button>
            <WhyUnavailable entry="reason_codes" id="why-counter-reasons" />
          </Show>
        }
      >
        <Show when={voidAct.phase() === "idle"} fallback={<PendingSettle act="void" phase={voidAct.phase()} amount="" onLeave={() => props.onVoided()} />}>
          <VoidBillPanel state={voiding} approver={approver} data-step="voidBillReason" onSubmit={(id) => voiding.voidBillReason(id)} />
        </Show>
        <RefusalPanel message={voidAct.refusal()} onDismiss={voidAct.dismiss} />
      </Show>
    </>
  );
}
