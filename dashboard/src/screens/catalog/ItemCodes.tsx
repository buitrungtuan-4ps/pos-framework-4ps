// An item's barcodes, PLU and open price, edited in the item's drawer (ADR-0222 decision 1). The
// barcodes are typed as one list, separated by spaces or commas; the cloud checks each check digit
// and refuses a code another item already carries, naming it, so the console sends what was typed
// and shows that refusal rather than guessing at the rule.

import { Show, createEffect, createSignal, type Accessor } from "solid-js";

import type { CatalogItem, ItemCodesBody } from "../../api/schemas";
import { MoneyField, TextField } from "../../components/ui";
import { t } from "../../i18n";

/** The drawer's draft of the codes of `item`, put back each time another item is opened. */
export function createCodesDraft(item: Accessor<CatalogItem | null>) {
  const [barcodes, setBarcodes] = createSignal("");
  const [plu, setPlu] = createSignal("");
  const [currency, setCurrency] = createSignal("VND");
  const [minimum, setMinimum] = createSignal<number | null>(null);
  const [maximum, setMaximum] = createSignal<number | null>(null);
  const reset = (opened: CatalogItem) => {
    setBarcodes((opened.barcodes ?? []).join(" "));
    setPlu(opened.plu ?? "");
    setCurrency(opened.open_price?.minimum.currency_code ?? "VND");
    setMinimum(opened.open_price?.minimum.amount_minor ?? null);
    setMaximum(opened.open_price?.maximum.amount_minor ?? null);
  };
  createEffect(() => {
    const opened = item();
    if (opened !== null) {
      reset(opened);
    }
  });
  return {
    barcodes, setBarcodes, plu, setPlu, currency, setCurrency, minimum, setMinimum, maximum, setMaximum,
    /** The draft as the cloud takes it: no open price unless both bounds are typed. */
    value: (): ItemCodesBody => {
      const code = currency().trim().toUpperCase();
      const low = minimum();
      const high = maximum();
      return {
        barcodes: barcodes().split(/[\s,]+/).filter((entry) => entry !== ""),
        plu: plu().trim() === "" ? null : plu().trim(),
        open_price:
          low === null || high === null
            ? null
            : {
                minimum: { currency_code: code, amount_minor: low },
                maximum: { currency_code: code, amount_minor: high },
              },
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
    </div>
  );
}
