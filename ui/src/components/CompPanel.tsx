// Giving a whole line away on the bill on screen (ADR-0219 decision 1): one tap opens the bill's
// lines, a second picks the one given away, and a reason the store publishes for a comp sends it,
// with a manager's badge and PIN above the reasons where the person may not comp alone. The amount
// is the edge's, the line's whole total, and the screen takes the bill as the edge answers with it.
// Offered while nothing has been paid towards the bill, as the discount is, and only where the store
// publishes a reason to cite.

import { For, Show, createSignal } from "solid-js";

import type { CheckResponse } from "../api/schemas";
import { t } from "../i18n";
import { useCompAct } from "../state/line-money-acts";
import { asksApprover, can } from "../state/permissions";
import { formatAmount, reasonsFor, state } from "../state/store";
import { ApproverFields, createApprover } from "./ApproverFields";
import { RefusalPanel } from "./RefusalPanel";

// The action a comp cites (ADR-0115): its own, so a store can hold reasons for giving food away
// apart from those for taking money off.
export const COMP = "REASON_ACTION_COMP";

// The bill's lines as buttons, the one picked marked.
function LinePicker(props: { readonly lineIds: readonly string[]; readonly picked: string | null; readonly onPick: (id: string) => void }) {
  return (
          <div class="mt-2 flex flex-col gap-2">
            <For each={props.lineIds}>
              {(id) => (
                <button
                  type="button"
                  class="flex min-h-touch items-center justify-between gap-2 rounded-token border border-line px-3 text-left"
                  classList={{ "border-accent": props.picked === id }}
                  aria-pressed={props.picked === id}
                  data-step="compLine"
                  onClick={() => props.onPick(id)}
                >
                  <span>{state.lines[id]?.name}</span>
                  <span class="tabular-nums">{formatAmount(state.lines[id]?.lineTotal ?? { currency_code: "", amount_minor: 0 })}</span>
                </button>
              )}
            </For>
          </div>
  );
}

export function CompPanel(props: {
  readonly billId: string | null;
  /** The order lines the bill covers. */
  readonly lineIds: readonly string[];
  /** True once the bill holds a payment, or while another act is out. */
  readonly disabled: boolean;
  readonly onComped: (totals: CheckResponse) => void;
}) {
  const approver = createApprover();
  const [open, setOpen] = createSignal(false);
  const [picked, setPicked] = createSignal<string | null>(null);
  const [comped, setComped] = createSignal<string | null>(null);
  const asks = () => asksApprover("billing.comp.apply");
  const close = () => {
    setOpen(false);
    setPicked(null);
    approver.clear();
  };
  const act = useCompAct((totals) => {
    setComped(state.lines[picked() ?? ""]?.name ?? null);
    props.onComped(totals);
    close();
  });
  const lines = () => props.lineIds.filter((id) => state.lines[id] !== undefined && state.lines[id]?.state !== "ORDER_LINE_STATE_VOIDED");
  const send = (reasonCodeId: string) => {
    const id = props.billId;
    const line = picked();
    if (id !== null && line !== null) {
      act.send(id, line, reasonCodeId, asks() ? approver.approval() : undefined, asks());
    }
  };
  return (
    <Show when={can("billing.comp.apply") && reasonsFor(COMP).length > 0 && lines().length > 0}>
      <Show
        when={open()}
        fallback={
          <button
            type="button"
            class="mt-2 min-h-touch w-full rounded-token border border-line text-sm disabled:opacity-50"
            disabled={props.billId === null || props.disabled}
            data-step="askComp"
            onClick={() => (setComped(null), setOpen(true))}
          >
            {t("pay.comp_ask")}
          </button>
        }
      >
        <div class="mt-3 rounded-token border border-line bg-surface p-3" data-outcome="comp-panel">
          <h2 class="font-semibold">{t("pay.comp_title")}</h2>
          <LinePicker lineIds={lines()} picked={picked()} onPick={setPicked} />
          <Show when={asks()}>
            <ApproverFields
              id="comp-approver"
              code={approver.code()}
              pin={approver.pin()}
              onCode={approver.setCode}
              onPin={approver.setPin}
              codeLabel={t("pay.approver_code")}
              pinLabel={t("pay.approver_pin")}
            />
          </Show>
          <p class="mt-3 text-sm text-ink-muted">{t("pay.comp_reason")}</p>
          <div class="mt-2 grid grid-cols-2 gap-2">
            <For each={reasonsFor(COMP)}>
              {(reason) => (
                <button
                  type="button"
                  class="min-h-touch rounded-token border border-line bg-surface px-3 text-left disabled:opacity-50"
                  disabled={picked() === null || (asks() && !approver.typed()) || act.phase() !== "idle"}
                  data-step="compReason"
                  onClick={() => send(reason.reason_code_id)}
                >
                  {reason.display_name}
                </button>
              )}
            </For>
          </div>
          <RefusalPanel message={act.refusal()} onDismiss={act.dismiss} />
          <button type="button" class="mt-3 min-h-touch rounded-token border border-line px-3 text-sm" onClick={close}>
            {t("common.cancel")}
          </button>
        </div>
      </Show>
      <Show when={comped()}>
        {(name) => (
          <p class="mt-1 text-sm text-ink-muted" role="status" data-outcome="line-comped">
            {t("pay.comp_done", { item: name() })}
          </p>
        )}
      </Show>
    </Show>
  );
}
