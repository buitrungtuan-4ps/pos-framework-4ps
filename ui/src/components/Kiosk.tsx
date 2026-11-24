// The kiosk's three components (ADR-0224 decision 6): `kiosk_menu`, `kiosk_cart` and `kiosk_done`.
// They read and change the kiosk's own order through `state/kiosk.ts`, and the menu the kiosk read
// when it started. Function declarations, as every catalogued implementation is (`package/registry`).

import { For, Show, createSignal } from "solid-js";

import type { MenuResponse } from "../api/schemas";
import { t } from "../i18n";
import { errorMessage } from "../lib/errors";
import { formatAmount } from "../state/store";
import { canRemove, card, fewer, lines, payByCard, queueNumber, refusal, ring, sendToPay, startOver } from "../state/kiosk";

const [kioskMenu, setKioskMenu] = createSignal<MenuResponse | null>(null);
export { setKioskMenu };

/** `kiosk_menu`: every item a guest may order, one tap each. */
export function KioskMenu() {
  const items = () => (kioskMenu()?.items ?? []).filter((item) => !item.sold_by_weight && item.open_price === undefined);
  return (
    <div class="grid grid-cols-2 gap-3 tablet:grid-cols-3" data-outcome="kiosk-menu">
      <For each={items()}>
        {(item) => (
          <button
            type="button"
            class="min-h-24 rounded-token border border-line bg-surface p-3 text-left"
            disabled={!item.available || item.sold_out}
            data-step="kioskItem"
            onClick={() => void ring(item)}
          >
            <span class="block text-lg font-semibold">{item.display_name}</span>
            <span class="block text-ink-muted">
              {item.available && !item.sold_out ? formatAmount(item.unit_price) : t("kiosk.sold_out")}
            </span>
          </button>
        )}
      </For>
    </div>
  );
}

/** `kiosk_cart`: the order so far, one more or fewer of each line, and sending it to be paid. */
export function KioskCart() {
  return (
    <section class="rounded-token border border-line bg-surface p-3" data-outcome="kiosk-cart">
      <h2 class="mb-2 text-lg font-semibold">{t("kiosk.your_order")}</h2>
      <Show when={lines().length > 0} fallback={<p class="text-ink-muted">{t("kiosk.empty")}</p>}>
        <For each={lines()}>
          {(line) => (
            <div class="flex items-center justify-between gap-2 py-2" data-outcome="kiosk-line">
              <span>{t("kiosk.line", { count: line.quantity, item: line.item.display_name })}</span>
              <span class="flex gap-2">
                <button
                  type="button"
                  class="min-h-11 min-w-11 rounded-token border border-line"
                  aria-label={t("kiosk.fewer", { item: line.item.display_name })}
                  disabled={line.quantity === 1 && !canRemove()}
                  onClick={() => void fewer(line)}
                >
                  −
                </button>
                <button
                  type="button"
                  class="min-h-11 min-w-11 rounded-token border border-line"
                  aria-label={t("kiosk.more", { item: line.item.display_name })}
                  onClick={() => void ring(line.item)}
                >
                  +
                </button>
              </span>
            </div>
          )}
        </For>
      </Show>
      <Show when={refusal()}>{(caught) => <p role="alert" class="text-danger">{errorMessage(caught())}</p>}</Show>
      <button
        type="button"
        class="mt-3 min-h-12 w-full rounded-token bg-primary font-semibold text-primary-ink"
        disabled={lines().length === 0}
        data-step="kioskSend"
        onClick={() => void sendToPay()}
      >
        {t("kiosk.send_to_pay")}
      </button>
    </section>
  );
}

/** `kiosk_done`: the number the order is called by, and a new order for the next guest. */
export function KioskDone() {
  return (
    <section class="mx-auto max-w-md p-6 text-center" data-outcome="kiosk-done">
      <p class="text-lg">{t("kiosk.your_number")}</p>
      <p class="my-4 text-6xl font-semibold tabular-nums" data-outcome="kiosk-number">
        {queueNumber() ?? ""}
      </p>
      <Show when={card() === "paid"} fallback={<p class="mb-6 text-ink-muted">{t(card() === "unknown" ? "kiosk.card_unknown" : "kiosk.pay_at_counter")}</p>}>
        <p class="mb-6 font-semibold" data-outcome="kiosk-paid">{t("kiosk.card_paid")}</p>
      </Show>
      {/* A card at the kiosk, through the terminal, for the whole bill (ADR-0230 decision 7). */}
      <Show when={card() === "offered" || card() === "busy" || card() === "declined"}>
        <Show when={card() === "declined"}>
          <p class="mb-2 text-danger" role="status">{t("card.declined")}</p>
        </Show>
        <button type="button" class="mb-4 min-h-12 w-full rounded-token border border-line bg-surface px-6 font-semibold disabled:opacity-50" disabled={card() === "busy"} data-step="kioskCard" onClick={() => void payByCard()}>
          {t(card() === "busy" ? "pay.paying" : "kiosk.pay_card")}
        </button>
      </Show>
      <button type="button" class="min-h-12 rounded-token bg-primary px-6 font-semibold text-primary-ink" onClick={startOver}>
        {t("kiosk.new_order")}
      </button>
    </section>
  );
}
