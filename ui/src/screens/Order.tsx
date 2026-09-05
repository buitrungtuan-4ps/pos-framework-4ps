import { For, Show, createSignal } from "solid-js";
import { useNavigate, useParams } from "@solidjs/router";

import { ApiError } from "../api/client";
import { t } from "../i18n";
import { tableStateKey } from "../i18n/labels";
import { formatMoney } from "../lib/money";
import type { LayoutButton, MenuItemResponse } from "../api/types";
import { addItem, fire, linesForTable, openBill, state, tableState } from "../state/store";

// A table's order: the running check on the left, the menu on the right (the tablet layout with a
// sliding bill). Tapping a menu item adds it optimistically; a fresh line can be fired to the
// kitchen. "Take payment" opens the bill and moves to the pay screen.
export function Order() {
  const params = useParams<{ id: string }>();
  const navigate = useNavigate();
  const [error, setError] = createSignal<string | null>(null);
  const label = () => params.id.replace(/^0+/, "") || params.id;

  const guard = async (run: () => Promise<void>) => {
    setError(null);
    try {
      await run();
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : t("common.store_error"));
    }
  };

  const takePayment = () =>
    guard(async () => {
      await openBill(params.id);
      navigate(`/table/${params.id}/pay`);
    });

  // Only categories that still have something to sell. A category whose every button names an item
  // the price book no longer carries would otherwise draw as an empty heading — the console arranged
  // it before the item was withdrawn, and a heading with nothing under it reads as a fault.
  const arranged = () =>
    state.layout.filter(
      (category) =>
        category.buttons.some(priced) ||
        category.subcategories.some((subcategory) => subcategory.buttons.some(priced)),
    );

  // Layout names the item; the price book prices it; the two meet only at the id (ADR-0066).
  const priced = (button: LayoutButton) =>
    state.menu.some((item) => item.menu_item_id === button.menu_item_id);

  const sellButton = (item: MenuItemResponse, caption: string) => (
    <button
      type="button"
      class="flex min-h-touch items-center justify-between rounded-token border border-line bg-surface px-3 py-2 text-left disabled:opacity-50"
      disabled={!item.available}
      data-step="addItem"
      onClick={() => void guard(() => addItem(params.id, item))}
    >
      <span>{caption}</span>
      <span class="tabular-nums text-ink-muted">
        {item.available ? formatMoney(item.unit_price) : t("order.unavailable")}
      </span>
    </button>
  );

  // An arranged button carries the caption the console wrote; the price comes from the price book, so
  // there is never a second price that can disagree with it. A button naming an item the price book
  // does not carry draws nothing rather than an unpriceable tap.
  const arrangedButton = (button: LayoutButton) => {
    const item = state.menu.find((entry) => entry.menu_item_id === button.menu_item_id);
    return <Show when={item}>{(found) => sellButton(found(), button.label)}</Show>;
  };

  return (
    <section class="grid gap-4 p-4 lg:grid-cols-[1fr_20rem]">
      <div>
        <div class="mb-3 flex items-center gap-3">
          <a href="/" class="text-sm text-ink-muted no-underline">
            {t("common.back_floor")}
          </a>
          {/* Seating a table ends here, on this table's order screen (ADR-0109). */}
          <h1 class="text-lg font-semibold" data-outcome="order-open">
            {t("common.table", { label: label() })}
          </h1>
          <span class="text-sm text-ink-muted">{t(tableStateKey(tableState(params.id)))}</span>
        </div>

        <Show when={error()}>
          {(message) => (
            <p class="mb-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
              {message()}
            </p>
          )}
        </Show>

        <ul class="flex flex-col gap-2">
          <For
            each={linesForTable(params.id)}
            fallback={<li class="text-ink-muted">{t("order.empty")}</li>}
          >
            {(line) => (
              <li
                class="flex items-center gap-3 rounded-token border border-line bg-surface p-3"
                data-outcome="line-added"
              >
                <span class="flex-1">{line.name}</span>
                <span class="tabular-nums">{formatMoney(line.lineTotal)}</span>
                <Show
                  when={line.state === "ORDER_LINE_STATE_ADDED"}
                  fallback={
                    <span class="text-sm text-ok" data-outcome="line-fired">
                      {t("order.fired")}
                    </span>
                  }
                >
                  <button
                    type="button"
                    class="rounded-token bg-accent px-3 py-1 text-accent-ink"
                    data-step="fire"
                    onClick={() => void guard(() => fire(line.orderLineId))}
                  >
                    {t("order.fire")}
                  </button>
                </Show>
              </li>
            )}
          </For>
        </ul>

        <button
          type="button"
          class="mt-4 min-h-touch w-full rounded-token bg-accent px-4 text-lg font-semibold text-accent-ink"
          data-step="takePayment"
          onClick={() => void takePayment()}
        >
          {t("order.take_payment")}
        </button>
      </div>

      <aside>
        <h2 class="mb-2 text-sm font-semibold text-ink-muted">{t("order.menu")}</h2>
        {/*
          Two ways to draw the same price book. When the console has arranged buttons on the `layout`
          node (ADR-0066, C4) the till groups by the categories it authored, in the order it authored
          them; when it has arranged nothing, the flat list is the honest fallback and is what the
          till drew before that node had a reader.
        */}
        <Show
          when={arranged().length > 0}
          fallback={
            <div class="grid grid-cols-2 gap-2 lg:grid-cols-1">
              <For
                each={state.menu}
                fallback={<p class="text-ink-muted">{t("order.menu_empty")}</p>}
              >
                {(item) => sellButton(item, item.display_name)}
              </For>
            </div>
          }
        >
          <For each={arranged()}>
            {(category) => (
              <section class="mb-4">
                <h3 class="mb-2 text-xs font-semibold uppercase tracking-wide text-ink-muted">
                  {category.name}
                </h3>
                <div class="grid grid-cols-2 gap-2 lg:grid-cols-1">
                  <For each={category.buttons}>{(button) => arrangedButton(button)}</For>
                </div>
                <For each={category.subcategories}>
                  {(subcategory) => (
                    <div class="mt-3">
                      <h4 class="mb-2 text-xs text-ink-muted">{subcategory.name}</h4>
                      <div class="grid grid-cols-2 gap-2 lg:grid-cols-1">
                        <For each={subcategory.buttons}>{(button) => arrangedButton(button)}</For>
                      </div>
                    </div>
                  )}
                </For>
              </section>
            )}
          </For>
        </Show>
      </aside>
    </section>
  );
}
