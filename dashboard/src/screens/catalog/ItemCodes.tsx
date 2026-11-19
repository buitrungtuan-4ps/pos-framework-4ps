// An item's barcodes, PLU and open price (ADR-0222 decision 1), and whether it is sold by weight
// with the tare its container weighs (ADR-0223), edited in the item's drawer. The
// barcodes are typed as one list, separated by spaces or commas; the cloud checks each check digit
// and refuses a code another item already carries, naming it, so the console sends what was typed
// and shows that refusal rather than guessing at the rule.

import { Show, createEffect, createSignal, type Accessor } from "solid-js";

import type { CatalogItem, ItemCodesBody } from "../../api/schemas";
import { CheckboxField, MoneyField, NumberField, TextField } from "../../components/ui";
import { t } from "../../i18n";

/** The drawer's draft of the codes of `item`, put back each time another item is opened. */
export function createCodesDraft(item: Accessor<CatalogItem | null>) {
  const [barcodes, setBarcodes] = createSignal("");
  const [plu, setPlu] = createSignal("");
  const [currency, setCurrency] = createSignal("VND");
  const [minimum, setMinimum] = createSignal<number | null>(null);
  const [maximum, setMaximum] = createSignal<number | null>(null);
  const [byWeight, setByWeight] = createSignal(false);
  const [tare, setTare] = createSignal<number | null>(null);
  const reset = (opened: CatalogItem) => {
    setBarcodes((opened.barcodes ?? []).join(" "));
    setPlu(opened.plu ?? "");
    setCurrency(opened.open_price?.minimum.currency_code ?? "VND");
    setMinimum(opened.open_price?.minimum.amount_minor ?? null);
    setMaximum(opened.open_price?.maximum.amount_minor ?? null);
    setByWeight(opened.sold_by === "SOLD_BY_WEIGHT");
    setTare(opened.tare_grams ?? null);
  };
  createEffect(() => {
    const opened = item();
    if (opened !== null) {
      reset(opened);
    }
  });
  return {
    barcodes, setBarcodes, plu, setPlu, currency, setCurrency, minimum, setMinimum, maximum, setMaximum,
    byWeight, setByWeight, tare, setTare,
    /** The draft as the cloud takes it: no open price unless both bounds are typed. */
    value: (): ItemCodesBody => {
      const code = currency().trim().toUpperCase();
      const low = minimum();
      const high = maximum();
      return {
        barcodes: barcodes().split(/[\s,]+/).filter((entry) => entry !== ""),
        plu: plu().trim() === "" ? undefined : plu().trim(),
        open_price:
          low === null || high === null
            ? undefined
            : {
                minimum: { currency_code: code, amount_minor: low },
                maximum: { currency_code: code, amount_minor: high },
              },
        // Sold by the kilogram, its price per kilogram; a tare only for such an item (ADR-0223).
        sold_by: byWeight() ? "SOLD_BY_WEIGHT" : undefined,
        tare_grams: byWeight() ? (tare() ?? undefined) : undefined,
      };
    },
  };
}

export function ItemCodesFields(props: { draft: ReturnType<typeof createCodesDraft> }) {
  const draft = props.draft;
  return (
    <div class="flex flex-col gap-2" data-testid="item-codes">
      <p class="text-xs text-ink-muted">{t("catalog.codesHint")}</p>
      <TextField label={t("catalog.barcodes")} value={draft.barcodes()} onInput={draft.setBarcodes} />
      <TextField label={t("catalog.plu")} value={draft.plu()} onInput={draft.setPlu} />
      <p class="text-xs text-ink-muted">{t("catalog.openPriceHint")}</p>
      <TextField label={t("catalog.openPriceCurrency")} value={draft.currency()} onInput={draft.setCurrency} />
      <Show when={draft.currency().trim().length === 3}>
        <MoneyField
          label={t("catalog.openPriceMinimum")}
          currencyCode={draft.currency().trim().toUpperCase()}
          value={draft.minimum()}
          onChange={draft.setMinimum}
        />
        <MoneyField
          label={t("catalog.openPriceMaximum")}
          currencyCode={draft.currency().trim().toUpperCase()}
          value={draft.maximum()}
          onChange={draft.setMaximum}
        />
      </Show>
      <CheckboxField label={t("catalog.soldByWeight")} hint={t("catalog.soldByWeightHint")} checked={draft.byWeight()} onChange={draft.setByWeight} />
      <Show when={draft.byWeight()}>
        <NumberField label={t("catalog.tareGrams")} value={draft.tare()} onChange={draft.setTare} min={0} max={99_998} />
      </Show>
    </div>
  );
}
