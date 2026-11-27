// New order's choice of the store's order types and sources (ADR-0260): the source first, the
// till's own unless staff pick one the store lists, then one tap on a type opens the order on that
// type's base kind's channel, which alone sets its price and tax. The store's default type is drawn
// first and filled.

import { For, Show, createSignal } from "solid-js";

import { t } from "../i18n";
import {
  channelOf,
  offeredSources,
  offeredTypes,
  sourceLabel,
  typeLabel,
  type OrderRecording,
} from "../state/order-channels";

export function OrderTypePicker(props: { onPick: (channel: string, recording: OrderRecording) => void }) {
  const [source, setSource] = createSignal<string | undefined>(undefined);
  return (
    <div data-component="order-type-picker">
      <Show when={offeredSources().length > 0}>
        <p class="mt-3 text-sm font-medium text-ink">{t("counter.source")}</p>
        <div class="mt-1 flex flex-wrap gap-2" role="radiogroup" aria-label={t("counter.source")}>
          <For each={[undefined, ...offeredSources()]}>
            {(offered) => (
              <button
                type="button"
                role="radio"
                aria-checked={source() === offered?.source}
                class="min-h-touch rounded-token border px-4"
                classList={{
                  "border-primary bg-primary text-primary-ink": source() === offered?.source,
                  "border-line text-ink": source() !== offered?.source,
                }}
                data-step="pickSource"
                onClick={() => setSource(offered?.source)}
              >
                {offered === undefined ? t("counter.source_pos") : sourceLabel(offered)}
              </button>
            )}
          </For>
        </div>
      </Show>
      <p class="mt-3 text-sm font-medium text-ink">{t("counter.order_type")}</p>
      <div class="mt-1 grid grid-cols-2 gap-2">
        <For each={offeredTypes()}>
          {(offered) => (
            <button
              type="button"
              class="min-h-money rounded-token border border-primary px-4 text-lg font-semibold text-ink"
              classList={{ "bg-primary text-primary-ink": offered.default }}
              data-step="pickType"
              data-order-type={offered.order_type}
              onClick={() =>
                props.onPick(channelOf(offered), { order_type: offered.order_type, sale_source: source() })
              }
            >
              {typeLabel(offered)}
            </button>
          )}
        </For>
      </div>
    </div>
  );
}
