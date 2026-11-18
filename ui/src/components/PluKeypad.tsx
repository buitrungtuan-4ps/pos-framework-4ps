// `plu_keypad` (ADR-0222 decision 5): the quick sale's PLU pad. A cashier keys an item's PLU, one to
// five digits, and Add sells it as a tap on its button would, through the till's code index; no
// round trip. Drawn only where the store sells by barcode.

import { For, Show, createSignal } from "solid-js";

import { t } from "../i18n";
import { resolveCode, scansListened } from "../state/codes";
import { useSell } from "../state/sell";
import { state } from "../state/store";

const DIGITS = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"] as const;
const PLU_DIGITS_MAX = 5;

export function PluKeypad() {
  const sell = useSell();
  const [plu, setPlu] = createSignal("");
  const [unknown, setUnknown] = createSignal(false);
  const add = () => {
    const itemId = resolveCode(plu());
    const item = state.menu.find((entry) => entry.menu_item_id === itemId);
    setUnknown(item === undefined);
    setPlu("");
    if (item !== undefined) {
      sell.sell(item);
    }
  };
  const key = "min-h-touch rounded-token border border-line bg-surface text-lg tabular-nums disabled:opacity-50";
  return (
    <Show when={scansListened()}>
      <div class="mb-3" data-outcome="plu-keypad">
        <p class="text-xl font-semibold tabular-nums">{plu() === "" ? t("order.plu") : plu()}</p>
        <Show when={unknown()}>
          <p class="text-sm text-danger" role="status">{t("order.plu_unknown")}</p>
        </Show>
        <div class="mt-2 grid grid-cols-3 gap-2">
          <For each={DIGITS}>
            {(digit) => (
              <button type="button" class={key} disabled={sell.disabled() || plu().length >= PLU_DIGITS_MAX} onClick={() => setPlu(plu() + digit)}>
                {digit}
              </button>
            )}
          </For>
          <button type="button" class={key} onClick={() => setPlu("")}>
            {t("order.plu_clear")}
          </button>
          <button type="button" class={`${key} bg-primary font-semibold text-primary-ink`} disabled={sell.disabled() || plu() === ""} onClick={add}>
            {t("order.plu_add")}
          </button>
        </div>
      </div>
    </Show>
  );
}
