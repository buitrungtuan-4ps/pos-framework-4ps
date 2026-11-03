// Voiding a bill before it settles (ADR-0115, §6): the act and its panel, for any screen that takes
// payment on a bill.
//
// Secondary to the tenders on purpose: it is the rare act, and it is the one that needs a second
// person. Always a manager — a bill is money whether or not the kitchen started — so unlike a line
// there is no shape of this the till lets through on its own, unless the store enforces each
// person's own set and the person holds the void directly (ADR-0158).
//
// The reason buttons stay disabled until the manager's badge and PIN are filled, which keeps the void
// at the three taps §6 allows a rare action (pay, void, reason) rather than spending one on a request
// that could only come back `403`.

import { For, Show, createSignal, type Accessor } from "solid-js";

import { t } from "../i18n";
import { asksApprover } from "../state/permissions";
import type { ApproverRequest } from "../api/schemas";
import { reasonsFor } from "../state/store";
import { ApproverFields, type Approver } from "./ApproverFields";

// The action voiding a whole bill cites (ADR-0115). A separate action from voiding a line, so a
// store can hold reasons for one and not the other — and the picker offers only the entries that
// declare this one.
export const VOID_BILL = "REASON_ACTION_VOID_BILL";

/** A screen's bill void: whether its panel is open, and the two taps that make it. */
export function useVoidBill(options: {
  billId: Accessor<string | null>;
  approver: Approver;
  /** The screen's error line: cleared when the panel opens. */
  report: (message: string | null) => void;
  /**
   * Sends the void as a money act (`../state/pay-acts`): the screen shows it pending, and closes
   * the panel once the edge commits it.
   */
  voidWith: (reasonCodeId: string, asks: boolean, approval: ApproverRequest | undefined) => void;
}) {
  const [open, setOpen] = createSignal(false);
  const asks = () => asksApprover("billing.bill.void");
  const ready = () => !asks() || options.approver.typed();

  const closeVoid = () => {
    setOpen(false);
    options.approver.clear();
  };

  // The first tap: open the picker. Nothing is written until a reason is chosen — the reason is
  // mandatory, not a question asked afterwards.
  const askVoidBill = () => {
    options.report(null);
    setOpen(true);
  };

  // The second tap: void the bill, citing this reason and the manager standing at the till. The
  // panel stays open on a refusal — a settled bill is refused by the bill machine, and reading why
  // is the point — and closes once the edge commits the void.
  const voidBillReason = (reasonCodeId: string) => {
    if (options.billId() !== null) {
      options.voidWith(reasonCodeId, asks(), asks() ? options.approver.approval() : undefined);
    }
  };

  return { open, asks, ready, closeVoid, askVoidBill, voidBillReason };
}

/**
 * The open void panel: the manager's fields where asked, the store's reasons, and Cancel. A reason
 * calls `onSubmit` with its id; `data-step` marks the reason buttons for the step gate.
 */
export function VoidBillPanel(props: {
  state: ReturnType<typeof useVoidBill>;
  approver: Approver;
  onSubmit: (reasonCodeId: string) => void;
  "data-step"?: string;
}) {
  return (
    <div class="mt-6 rounded-token border border-line bg-surface p-3">
      <h2 class="font-semibold">{t("pay.void_title")}</h2>
      <Show when={props.state.asks()}>
        <p class="mt-1 text-sm text-ink-muted">{t("pay.void_manager")}</p>
        <ApproverFields
          id="void-bill-approver"
          code={props.approver.code()}
          pin={props.approver.pin()}
          onCode={props.approver.setCode}
          onPin={props.approver.setPin}
          codeLabel={t("pay.approver_code")}
          pinLabel={t("pay.approver_pin")}
        />
      </Show>
      <p class="mt-3 text-sm text-ink-muted">{t("pay.void_reason")}</p>
      <div class="mt-2 grid grid-cols-2 gap-2">
        <For each={reasonsFor(VOID_BILL)}>
          {(reason) => (
            <button
              type="button"
              class="min-h-touch rounded-token border border-line bg-surface px-3 text-left disabled:opacity-50"
              disabled={!props.state.ready()}
              data-step={props["data-step"]}
              onClick={() => props.onSubmit(reason.reason_code_id)}
            >
              {reason.display_name}
            </button>
          )}
        </For>
      </div>
      <button
        type="button"
        class="mt-3 min-h-touch rounded-token border border-line px-3 text-sm"
        onClick={() => props.state.closeVoid()}
      >
        {t("common.cancel")}
      </button>
    </div>
  );
}
