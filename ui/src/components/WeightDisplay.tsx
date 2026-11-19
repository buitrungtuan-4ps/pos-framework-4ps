// `weight_display` (ADR-0223): the weight of the item being weighed. It shows what the scale beside
// the till reads, read from the edge every half second while it is open, and weighs the item on the
// scale's settled weight; where the store allows it, a weight may be typed in grams instead. The edge
// takes the item's tare off, prices the net grams per kilogram and records where the weight came
// from. A weight is taken on a counter order, which the order screen has once its first line is in.

import { Show, createSignal, onCleanup } from "solid-js";

import { api } from "../api/client";
import type { ScaleResponse } from "../api/schemas";
import { t } from "../i18n";
import { errorMessage } from "../lib/errors";
import { useSell } from "../state/sell";
import { loadLiveOrders } from "../state/store";
import { kilograms, manualWeightAllowed, minGrams, stopWeighing, weigh, weighingItem } from "../state/weigh";
import { Keypad } from "./Keypad";

const READ_EVERY_MS = 500;

export function WeightDisplay() {
  const sell = useSell();
  const [reading, setReading] = createSignal<ScaleResponse | null>(null);
  const [typed, setTyped] = createSignal("");
  const [refusal, setRefusal] = createSignal<string | null>(null);
  const read = () => {
    if (weighingItem() === null) return;
    api.readScale().then(
      (answer) => setReading(answer),
      () => setReading(null),
    );
  };
  const timer = setInterval(read, READ_EVERY_MS);
  onCleanup(() => clearInterval(timer));
  const settled = () => reading()?.state === "SCALE_STATE_STABLE";
  const grams = () => (typed() === "" ? null : Number.parseInt(typed(), 10));
  const send = (typedGrams: number | null) => {
    const item = weighingItem();
    const orderId = sell.orderId();
    if (item === null || orderId === undefined) return;
    setRefusal(null);
    weigh(orderId, item, typedGrams, () => void loadLiveOrders()).then(
      () => setTyped(""),
      (caught: unknown) => setRefusal(errorMessage(caught)),
    );
  };
  const close = () => {
    setTyped("");
    setRefusal(null);
    stopWeighing();
  };
  return (
    <Show when={weighingItem()}>
      {(item) => (
        <div class="mb-3 rounded-token border border-line bg-surface p-3" data-outcome="weight-display">
          <p class="font-semibold">{t("order.weigh_title", { item: item().display_name })}</p>
          <p class="text-2xl font-semibold tabular-nums" data-outcome="scale-reading">
            {settled() ? kilograms(reading()?.net_grams ?? 0) : t(reading() === null ? "order.scale_none" : "order.scale_moving")}
          </p>
          <Show when={sell.orderId() === undefined}>
            <p class="text-sm text-ink-muted">{t("order.weigh_needs_order")}</p>
          </Show>
          <Show when={refusal()}>{(message) => <p class="text-sm text-danger" role="status">{message()}</p>}</Show>
          <Show when={manualWeightAllowed()}>
            <p class="mt-2 text-sm">{t("order.weigh_typed", { grams: typed() === "" ? "—" : typed(), least: minGrams() })}</p>
            <Keypad value={typed()} onChange={setTyped} />
          </Show>
          <div class="mt-2 grid grid-cols-2 gap-2">
            <button
              type="button"
              class="min-h-touch rounded-token bg-primary font-semibold text-primary-ink disabled:opacity-50"
              disabled={sell.disabled() || sell.orderId() === undefined || (grams() === null && !settled())}
              onClick={() => send(grams())}
            >
              {t(grams() === null ? "order.weigh_add" : "order.weigh_add_typed")}
            </button>
            <button type="button" class="min-h-touch rounded-token border border-line" onClick={close}>
              {t("common.cancel")}
            </button>
          </div>
        </div>
      )}
    </Show>
  );
}
