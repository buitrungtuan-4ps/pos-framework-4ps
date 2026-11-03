// A numeric keypad for entering money on a till with no keyboard.
//
// `docs/ui-ux.md` has asked for "a large numeric keypad for cash" since it was written, and the
// quick-cash denomination buttons beside it were built while this was not. The gap is not cosmetic.
// `inputmode="numeric"` asks the *platform* for a keypad, which a phone and a tablet always provide
// and a fixed 13"+ terminal — the class §9 names, and the machine a counter is run from — provides
// only if its on-screen keyboard is enabled. Where it is not, the float field cannot be filled and
// the shift cannot be opened. Asking the platform is the dependency; drawing the keys is not.
//
// Digits only, with no decimal key, because that is what the fields it serves accept: `parseWhole`
// takes whole units and multiplies by the currency's exponent, so a cashier types 100000 for a
// hundred thousand đồng and 261 for two hundred and sixty-one rupees. A decimal key would offer a
// figure the parser refuses.
//
// Where the currency has no minor unit (đồng, yen) every amount a cashier types ends in zeros, so
// the pad adds a `000` key: 150,000₫ is `1`, `5`, `000` rather than six taps. It sits in a fourth
// column, beside a wide `0`, so the pad keeps its height on a phone. Where the field is a pile of
// cash being counted or handed over (`notes`), a row of the store's own banknotes above the digits
// adds one note to what is typed: the float, cash paid in or out, and the count.
//
// It does not replace the text input it sits under. A terminal with a USB keyboard, a tablet with an
// OS keypad and the browser gate all still type into that field, and taking it away would trade one
// device class's problem for another's.

import { For, Show } from "solid-js";

import { t } from "../i18n";
import { money } from "../lib/money";
import { cashDenominations, currencyExponent, formatAmount, storeCurrency } from "../state/store";

/** The keys, in calculator order: 7-8-9 on top, zero on the bottom row. */
const DIGITS = ["7", "8", "9", "4", "5", "6", "1", "2", "3"] as const;

/** How many of the store's notes the `notes` row offers: its largest, so one row fits a phone. */
const NOTE_KEYS = 4;

export function Keypad(props: {
  /** The digits entered so far. */
  value: string;
  /** Called with the next value; the caller owns the state, as it owns the field. */
  onChange: (next: string) => void;
  /** Offer the store's banknotes as keys that add one note to the amount. */
  notes?: boolean;
  /** The step gate's handle, when a declared flow presses this. */
  "data-step"?: string;
}) {
  const press = (digits: string) => {
    // A leading zero is dropped rather than accumulated: `0` then `5` is five, not "05", which
    // `parseWhole` would accept and nobody means — and `000` on nothing is nothing, not "000".
    const next = (props.value === "0" ? "" : props.value) + digits;
    props.onChange(/^0+$/.test(next) ? "0" : next);
  };
  const thousands = () => currencyExponent() === 0;
  // The store's notes in whole units, which is what the field holds; a coin smaller than one unit
  // is not a figure the field can type, so it is left off.
  const unit = () => 10 ** currencyExponent();
  const notes = () =>
    [...cashDenominations()]
      .filter((note) => note > 0 && note % unit() === 0)
      .sort((a, b) => a - b)
      .slice(-NOTE_KEYS);
  // One more note on what is typed: a field holding no figure yet holds that note. Whole units,
  // which `parseWhole` reads, so adding is integer arithmetic on what the cashier sees.
  const addNote = (note: number) => {
    const typed = /^\d+$/.test(props.value) ? Number(props.value) : 0;
    props.onChange(String(typed + note / unit()));
  };
  // 56px keys and 8px gaps, the floor `docs/ui-ux.md` §2 sets for a cash keypad — above the 48px
  // touch minimum because this one is pressed repeatedly and under time pressure.
  const key =
    "min-h-[56px] rounded-token border border-line bg-surface-raised text-xl font-semibold text-ink active:bg-surface focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent";

  // Pressing a key must not take the caret out of the field it is filling. `pointerdown` is where a
  // button steals focus, so that is where it is refused — `click` still fires and still types.
  const hold = (event: PointerEvent) => event.preventDefault();

  // A grid of single characters says nothing on its own to a screen reader, so the group names it.
  return (
    <div role="group" aria-label={t("keypad.aria_label")} data-step={props["data-step"]}>
      <Show when={props.notes && notes().length > 0}>
        <div class="mt-2 grid grid-cols-4 gap-2" data-outcome="keypad-notes">
          <For each={notes()}>
            {(note) => (
              <button
                type="button"
                class="min-h-touch rounded-token border border-line bg-surface text-sm tabular-nums"
                onPointerDown={hold}
                onClick={() => addNote(note)}
              >
                {t("keypad.add_note", { amount: formatAmount(money(storeCurrency(), note)) })}
              </button>
            )}
          </For>
        </div>
      </Show>
      <div class="mt-2 grid gap-2" classList={{ "grid-cols-3": !thousands(), "grid-cols-4": thousands() }}>
        <For each={DIGITS}>
          {(digit) => (
            <button type="button" class={key} onPointerDown={hold} onClick={() => press(digit)}>
              {digit}
            </button>
          )}
        </For>
        <button
          type="button"
          class={key}
          classList={{ "col-start-4 row-start-2": thousands() }}
          onPointerDown={hold}
          aria-label={t("keypad.clear")}
          onClick={() => props.onChange("")}
        >
          {t("keypad.clear_short")}
        </button>
        <button
          type="button"
          class={key}
          classList={{ "col-span-3": thousands() }}
          onPointerDown={hold}
          onClick={() => press("0")}
        >
          0
        </button>
        <button
          type="button"
          class={key}
          classList={{ "col-start-4 row-start-1": thousands() }}
          onPointerDown={hold}
          aria-label={t("keypad.backspace")}
          onClick={() => props.onChange(props.value.slice(0, -1))}
        >
          ⌫
        </button>
        <Show when={thousands()}>
          <button
            type="button"
            class={`${key} col-start-4 row-start-3 row-span-2`}
            onPointerDown={hold}
            onClick={() => press("000")}
          >
            000
          </button>
        </Show>
      </div>
    </div>
  );
}
