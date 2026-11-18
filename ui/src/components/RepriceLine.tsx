// Changing a line's price before its bill opens (ADR-0219 decision 2): **Price** on the line opens
// a keypad for the new price of one, a manager's badge and PIN where the person may not override
// alone, and the reasons the store publishes for a price override; a reason sends it. The edge
// bounds the price (above zero, at most the store's published multiple of the menu price) and
// answers with the line's new total, which the line shows. Offered only while the order has no bill,
// on a line the edge has, and only where the store publishes a reason to cite.

import { For, Show, createSignal } from "solid-js";

import { t } from "../i18n";
import { money, parseWhole } from "../lib/money";
import { useRepriceAct } from "../state/line-money-acts";
import { asksApprover, can } from "../state/permissions";
import { currencyExponent, reasonsFor, storeCurrency, type OrderLine } from "../state/store";
import { ApproverFields, createApprover } from "./ApproverFields";
import { Keypad } from "./Keypad";
import { RefusalPanel } from "./RefusalPanel";

// The action a price override cites (ADR-0219): its own reasons, apart from a discount's.
export const PRICE_OVERRIDE = "REASON_ACTION_PRICE_OVERRIDE";

export function RepriceLine(props: { readonly line: OrderLine; readonly billOpen: boolean }) {
  const approver = createApprover();
  const [open, setOpen] = createSignal(false);
  const [typed, setTyped] = createSignal("");
  const asks = () => asksApprover("billing.price.override");
  const price = () => parseWhole(typed(), currencyExponent());
  const close = () => {
    setOpen(false);
    setTyped("");
    approver.clear();
  };
  const act = useRepriceAct(close);
  const offered = () =>
    can("billing.price.override") &&
    !props.billOpen &&
    props.line.state !== "ORDER_LINE_STATE_VOIDED" &&
    !props.line.orderLineId.startsWith("PENDING") &&
    reasonsFor(PRICE_OVERRIDE).length > 0;
  const send = (reasonCodeId: string) => {
    const minor = price();
    if (minor !== null && minor > 0) {
      act.send({
        orderId: props.line.orderId,
        orderLineId: props.line.orderLineId,
        unitPrice: money(storeCurrency(), minor),
        reasonCodeId,
        approval: asks() ? approver.approval() : undefined,
        asks: asks(),
        billOpen: props.billOpen,
      });
    }
  };
  return (
    <Show when={offered()}>
      <button
        type="button"
        class="min-h-touch rounded-token border border-line px-3 text-sm text-ink-muted"
        aria-expanded={open()}
        data-step="askReprice"
        onClick={() => (open() ? close() : setOpen(true))}
      >
        {t("order.reprice")}
      </button>
      <Show when={open()}>
        <div class="basis-full rounded-token border border-line bg-surface p-3" data-outcome="reprice-panel">
          <p class="text-sm text-ink-muted">{t("order.reprice_title", { item: props.line.name })}</p>
          <p class="text-xl font-semibold tabular-nums" data-outcome="reprice-typed">
            {typed() === "" ? "—" : typed()}
          </p>
          <Keypad value={typed()} onChange={setTyped} data-step="repriceKeypad" />
          <Show when={asks()}>
            <ApproverFields
              id="reprice-approver"
              code={approver.code()}
              pin={approver.pin()}
              onCode={approver.setCode}
              onPin={approver.setPin}
              codeLabel={t("order.approver_code")}
              pinLabel={t("order.approver_pin")}
            />
          </Show>
          <p class="mt-3 text-sm text-ink-muted">{t("order.reprice_reason")}</p>
          <div class="mt-2 grid grid-cols-2 gap-2">
            <For each={reasonsFor(PRICE_OVERRIDE)}>
              {(reason) => (
                <button
                  type="button"
                  class="min-h-touch rounded-token border border-line bg-surface px-3 text-left disabled:opacity-50"
                  disabled={(price() ?? 0) <= 0 || (asks() && !approver.typed()) || act.phase() !== "idle"}
                  data-step="repriceReason"
                  onClick={() => send(reason.reason_code_id)}
                >
                  {reason.display_name}
                </button>
              )}
            </For>
          </div>
          <RefusalPanel message={act.refusal()} onDismiss={act.dismiss} />
        </div>
      </Show>
    </Show>
  );
}
