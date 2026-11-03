// A pile of cash entered on the Shift screen: the opening float, the count, and a drop to the safe.
// Where the store counts by note (ADR-0186), it is the note-by-note counter and its total; where its
// counting list is empty, the one typed amount it always was, with the money keypad and its note keys.

import { For, Show, type JSX } from "solid-js";

import { t } from "../i18n";
import { money, type Money } from "../lib/money";
import type { CountBreakdown } from "../api/schemas";
import { countDenominations, type CounterValue } from "../state/cash-count";
import { formatAmount, storeCurrency } from "../state/store";
import { DenominationCounter } from "./DenominationCounter";
import { Keypad } from "./Keypad";

export function CountField(props: {
  /** The typed field's id, which its label names. */
  id: string;
  label: string;
  /** The typed amount, where there is no counter. */
  text: string;
  onText: (text: string) => void;
  /** The counter's count, where there is one. */
  count: CounterValue;
  onCount: (count: CounterValue) => void;
  /** The step gate's handle, on the keypad. */
  step: string;
  /** What the store expects of this pile, said above the counter, which fills nothing in. */
  target?: Money;
  /** Said between the label and the field. */
  children?: JSX.Element;
}) {
  return (
    <Show
      when={countDenominations().length > 0}
      fallback={
        <>
          <label class="mt-4 block text-sm text-ink-muted" for={props.id}>
            {props.label}
          </label>
          {props.children}
          <input
            id={props.id}
            inputmode="numeric"
            class="mt-1 w-full rounded-token border border-line bg-surface p-3 tabular-nums"
            value={props.text}
            onInput={(event) => props.onText(event.currentTarget.value)}
          />
          <Keypad value={props.text} onChange={props.onText} notes data-step={props.step} />
        </>
      }
    >
      <p class="mt-4 text-sm text-ink-muted">{props.label}</p>
      {props.children}
      <Show when={props.target}>
        {(target) => (
          <p class="mt-1 text-sm tabular-nums" data-outcome="count-target">
            {t("counter.target", { amount: formatAmount(target()) })}
          </p>
        )}
      </Show>
      <DenominationCounter
        denominations={countDenominations()}
        value={props.count}
        onChange={props.onCount}
        data-step={props.step}
      />
    </Show>
  );
}

// The count note by note on the close card (ADR-0186 decision 3), as the edge answered the close:
// each note counted, how many and their value, then the loose amount. Nothing for a count typed as
// one amount.
export function CountedRows(props: { counting?: CountBreakdown }) {
  const amount = (minor: number) => formatAmount(money(storeCurrency(), minor));
  return (
    <Show when={props.counting}>
      {(counting) => (
        <ul class="mt-2 text-sm text-ink-muted tabular-nums" data-outcome="counted-rows">
          <For each={counting().denomination_counts ?? []}>
            {(row) => (
              <li>
                {t("counter.row", {
                  note: formatAmount(row.denomination),
                  quantity: row.quantity,
                  amount: amount(row.denomination.amount_minor * row.quantity),
                })}
              </li>
            )}
          </For>
          <Show when={counting().uncounted_amount}>
            {(uncounted) => (
              <li>
                {t("counter.uncounted")} {formatAmount(uncounted())}
              </li>
            )}
          </Show>
        </ul>
      )}
    </Show>
  );
}
