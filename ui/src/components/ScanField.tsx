// `scan_field` (ADR-0222 decision 5): the quick sale's code field. A scanner's digits anywhere on
// the screen, or a code typed here and Enter, sell the item the till's code index names, as a tap on
// its button would. A code the index does not hold goes to the edge on a counter order
// (`sales.line.scan`), which reads a scale's variable-measure label by the store's layout and prices
// the line itself; anywhere else it is unknown. Drawn only where the store sells by barcode.

import { Show, createSignal } from "solid-js";

import { api } from "../api/client";
import type { MenuItemResponse } from "../api/schemas";
import { t } from "../i18n";
import { errorMessage } from "../lib/errors";
import { resolveCode, scansListened } from "../state/codes";
import { editWrite } from "../state/edge-write";
import { useSell } from "../state/sell";
import { loadLiveOrders, state } from "../state/store";
import { ScanListener } from "./ScanListener";

export function ScanField() {
  const sell = useSell();
  const [code, setCode] = createSignal("");
  const [refusal, setRefusal] = createSignal<string | null>(null);
  const onItem = (item: MenuItemResponse) => {
    setRefusal(null);
    sell.sell(item);
  };
  const enter = (typed: string) => {
    setCode("");
    const itemId = resolveCode(typed);
    const item = state.menu.find((entry) => entry.menu_item_id === itemId);
    const orderId = sell.orderId();
    if (item !== undefined) {
      onItem(item);
    } else if (orderId !== undefined && /^\d{1,14}$/.test(typed)) {
      setRefusal(null);
      void editWrite("sales.line.scan", { send: () => api.scanLine(orderId, typed), reread: () => void loadLiveOrders() }).catch((caught: unknown) =>
        setRefusal(errorMessage(caught)),
      );
    } else {
      setRefusal(t("order.scan_unknown", { code: typed }));
    }
  };
  return (
    <Show when={scansListened()}>
      <ScanListener disabled={sell.disabled()} onItem={onItem} onUnknown={enter} />
      <form class="mb-2 flex gap-2" onSubmit={(event) => { event.preventDefault(); if (code().trim() !== "") enter(code().trim()); }}>
        <input
          class="min-h-touch flex-1 rounded-token border border-line px-3 tabular-nums"
          inputmode="numeric"
          aria-label={t("order.scan_field")}
          placeholder={t("order.scan_field")}
          disabled={sell.disabled()}
          value={code()}
          onInput={(event) => setCode(event.currentTarget.value)}
          data-outcome="scan-field"
        />
      </form>
      <Show when={refusal()}>
        {(message) => <p class="mb-2 text-sm text-danger" role="status" data-outcome="scan-unknown">{message()}</p>}
      </Show>
    </Show>
  );
}
