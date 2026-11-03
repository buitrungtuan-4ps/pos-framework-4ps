// A drawer counted note by note (ADR-0186 decision 3): one row per note and coin in the store's
// counting list, each a quantity with − and + keys and the row's value; then "Other / loose" for
// anything the list does not name; then the running total in large type. The total is what the act
// records, so a count can no longer be added up wrong on paper.
//
// The largest note comes first, which is how a cashier counts a drawer, and the total stays in view
// at the foot of the screen while the rows scroll. A quantity is typed on the
// keypad under the rows, into the row last tapped, or on a keyboard into the row's own field. The
// component holds no state of its own beyond which row the keypad types into: the caller owns the
// value, as it owns a field, and gets a whole new one on every change.

import { For, createSignal } from "solid-js";

import { t } from "../i18n";
import { money } from "../lib/money";
import type { CounterValue } from "../state/cash-count";
import { currencyExponent, formatAmount, storeCurrency } from "../state/store";
import { Keypad } from "./Keypad";

/** More of one note than any drawer holds: a quantity typed past it stops there. */
const MOST = 9999;

// 56px steps and fields, the floor `docs/ui-ux.md` §2 sets for a cash keypad: a count is tapped often.
const STEP =
  "min-h-[56px] min-w-[56px] rounded-token border border-line bg-surface-raised text-xl font-semibold text-ink active:bg-surface";
const FIELD =
  "min-h-[56px] rounded-token border border-line bg-surface p-2 text-center text-lg tabular-nums";
const ROW = "flex items-center gap-2 rounded-token border bg-surface p-2";

/** A field's digits as a number, and anything else as none. */
const digits = (text: string) => (/^\d+$/.test(text) ? Number(text) : 0);
/** A quantity as its field shows it: nothing for none. */
const quantityText = (quantity: number) => (quantity === 0 ? "" : String(quantity));

/** Which field the keypad types into: a note or coin, by its value, or the loose amount. */
type Target = number | "uncounted";

export function DenominationCounter(props: {
  /** The notes and coins to count by, in minor units, in any order. */
  denominations: readonly number[];
  /** What is counted so far. */
  value: CounterValue;
  /** Called with the whole count after every change. */
  onChange: (next: CounterValue) => void;
  /** The step gate's handle, on the keypad. */
  "data-step"?: string;
}) {
  const largestFirst = () => [...props.denominations].sort((a, b) => b - a);
  const [target, setTarget] = createSignal<Target | null>(null);
  const typing = (): Target => target() ?? largestFirst()[0] ?? "uncounted";
  const unit = () => 10 ** currencyExponent();
  const amount = (minor: number) => formatAmount(money(storeCurrency(), minor));

  const quantityOf = (note: number) =>
    props.value.rows.find((row) => row.denomination.amount_minor === note)?.quantity ?? 0;

  // The whole count again, with one note's quantity or the loose amount changed, in the list's order.
  const change = (note: Target, next: number) => {
    const quantities = props.denominations.map((each) => ({
      note: each,
      quantity: each === note ? next : quantityOf(each),
    }));
    const uncounted = note === "uncounted" ? next : props.value.uncounted;
    const rows = quantities
      .filter((row) => row.quantity > 0)
      .map((row) => ({ denomination: money(storeCurrency(), row.note), quantity: row.quantity }));
    const total = quantities.reduce((sum, row) => sum + row.note * row.quantity, uncounted);
    props.onChange({ total, rows, uncounted });
  };
  const setQuantity = (note: number, quantity: number) =>
    change(note, Math.min(MOST, Math.max(0, quantity)));

  // A quantity and the loose amount as a field shows them: nothing for none, the loose amount in
  // whole units like every other money field.
  const uncountedText = () =>
    props.value.uncounted === 0 ? "" : String(props.value.uncounted / unit());
  const typeUncounted = (text: string) => change("uncounted", digits(text) * unit());
  const keypadText = () => {
    const into = typing();
    return into === "uncounted" ? uncountedText() : quantityText(quantityOf(into));
  };
  const typeKeypad = (text: string) => {
    const into = typing();
    if (into === "uncounted") {
      typeUncounted(text);
    } else {
      setQuantity(into, digits(text));
    }
  };

  return (
    <div data-outcome="denomination-counter">
      <div class="mt-2 grid gap-2">
        <For each={largestFirst()}>
          {(note) => (
            <NoteRow
              label={amount(note)}
              value={amount(note * quantityOf(note))}
              quantity={quantityOf(note)}
              active={typing() === note}
              onFocus={() => setTarget(note)}
              onQuantity={(quantity) => setQuantity(note, quantity)}
            />
          )}
        </For>
        <label
          class={ROW}
          classList={{
            "border-primary": typing() === "uncounted",
            "border-line": typing() !== "uncounted",
          }}
        >
          <span class="font-semibold">{t("counter.uncounted")}</span>
          <input
            inputmode="numeric"
            class={`${FIELD} ml-auto w-32`}
            value={uncountedText()}
            data-outcome="counter-uncounted"
            onFocus={() => setTarget("uncounted")}
            onInput={(event) => typeUncounted(event.currentTarget.value)}
          />
        </label>
      </div>
      <p
        class="sticky bottom-0 mt-3 bg-surface py-2 text-3xl font-semibold tabular-nums"
        role="status"
        data-outcome="counter-total"
      >
        {t("counter.total", { amount: amount(props.value.total) })}
      </p>
      <Keypad value={keypadText()} onChange={typeKeypad} data-step={props["data-step"]} />
    </div>
  );
}

// One note or coin: its face, − and + around how many, and what they come to.
function NoteRow(props: {
  label: string;
  value: string;
  quantity: number;
  active: boolean;
  onFocus: () => void;
  onQuantity: (quantity: number) => void;
}) {
  // A − or + also points the keypad at this row, as a tap in its field does.
  const step = (by: number) => {
    props.onFocus();
    props.onQuantity(props.quantity + by);
  };
  return (
    <div class={ROW} classList={{ "border-primary": props.active, "border-line": !props.active }}>
      <span class="w-24 shrink-0 font-semibold tabular-nums">{props.label}</span>
      <button
        type="button"
        class={STEP}
        aria-label={t("counter.less", { note: props.label })}
        onClick={() => step(-1)}
      >
        −
      </button>
      <input
        inputmode="numeric"
        class={`${FIELD} w-16`}
        aria-label={t("counter.quantity", { note: props.label })}
        value={quantityText(props.quantity)}
        onFocus={() => props.onFocus()}
        onInput={(event) => props.onQuantity(digits(event.currentTarget.value))}
      />
      <button
        type="button"
        class={STEP}
        aria-label={t("counter.more", { note: props.label })}
        onClick={() => step(1)}
      >
        +
      </button>
      <span class="ml-auto text-ink-muted tabular-nums" data-outcome="counter-row-value">
        {props.value}
      </span>
    </div>
  );
}
