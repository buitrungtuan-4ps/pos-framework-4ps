// The price of an open-price item, typed before it sells (ADR-0222 decision 4): within the item's
// published bounds it needs no manager; the edge checks the bounds again and refuses outside them.

import { Show, createSignal } from "solid-js";

import { t } from "../i18n";
import { money, parseWhole } from "../lib/money";
import { cancelPricing, priceTyped, pricingItem } from "../state/sell";
import { currencyExponent, formatAmount, storeCurrency } from "../state/store";
import { Keypad } from "./Keypad";

export function OpenPricePrompt() {
  const [typed, setTyped] = createSignal("");
  const minor = () => parseWhole(typed(), currencyExponent());
  const within = () => {
    const bounds = pricingItem()?.open_price;
    const value = minor();
    return bounds !== undefined && value !== null && value >= bounds.minimum.amount_minor && value <= bounds.maximum.amount_minor;
  };
  const done = (sell: boolean) => {
    const value = minor();
    setTyped("");
    if (sell && value !== null) {
      priceTyped(money(storeCurrency(), value));
    } else {
      cancelPricing();
    }
  };
  return (
    <Show when={pricingItem()}>
      {(item) => (
        <div class="mb-3 rounded-token border border-line bg-surface p-3" data-outcome="open-price">
          <p class="font-semibold">{t("order.open_price_title", { item: item().display_name })}</p>
          <Show when={item().open_price}>
            {(bounds) => (
              <p class="text-sm text-ink-muted">
                {t("order.open_price_bounds", { minimum: formatAmount(bounds().minimum), maximum: formatAmount(bounds().maximum) })}
              </p>
            )}
          </Show>
          <p class="text-xl font-semibold tabular-nums">{typed() === "" ? "—" : typed()}</p>
          <Keypad value={typed()} onChange={setTyped} />
          <div class="mt-2 grid grid-cols-2 gap-2">
            <button type="button" class="min-h-touch rounded-token bg-primary font-semibold text-primary-ink disabled:opacity-50" disabled={!within()} onClick={() => done(true)}>
              {t("order.open_price_add")}
            </button>
            <button type="button" class="min-h-touch rounded-token border border-line" onClick={() => done(false)}>
              {t("common.cancel")}
            </button>
          </div>
        </div>
      )}
    </Show>
  );
}
