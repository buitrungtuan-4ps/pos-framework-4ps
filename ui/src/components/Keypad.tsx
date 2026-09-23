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
// It does not replace the text input it sits under. A terminal with a USB keyboard, a tablet with an
// OS keypad and the browser gate all still type into that field, and taking it away would trade one
// device class's problem for another's.

import { For } from "solid-js";

import { t } from "../i18n";

/** The keys, in calculator order: 7-8-9 on top, zero on the bottom row. */
const DIGITS = ["7", "8", "9", "4", "5", "6", "1", "2", "3"] as const;

export function Keypad(props: {
  /** The digits entered so far. */
  value: string;
  /** Called with the next value; the caller owns the state, as it owns the field. */
  onChange: (next: string) => void;
  /** The step gate's handle, when a declared flow presses this. */
  "data-step"?: string;
}) {
  const press = (digit: string) => {
    // A leading zero is dropped rather than accumulated: `0` then `5` is five, not "05", which
    // `parseWhole` would accept and nobody means.
    props.onChange(props.value === "0" ? digit : props.value + digit);
  };
  // 56px keys and 8px gaps, the floor `docs/ui-ux.md` §2 sets for a cash keypad — above the 48px
  // touch minimum because this one is pressed repeatedly and under time pressure.
  const key =
    "min-h-[56px] rounded-token border border-line bg-surface-raised text-xl font-semibold text-ink active:bg-surface";
  return (
    <div class="mt-2 grid grid-cols-3 gap-2" data-step={props["data-step"]}>
      <For each={DIGITS}>
        {(digit) => (
          <button type="button" class={key} onClick={() => press(digit)}>
            {digit}
          </button>
        )}
      </For>
      <button
        type="button"
        class={key}
        aria-label={t("keypad.clear")}
        onClick={() => props.onChange("")}
      >
        {t("keypad.clear_short")}
      </button>
      <button type="button" class={key} onClick={() => press("0")}>
        0
      </button>
      <button
        type="button"
        class={key}
        aria-label={t("keypad.backspace")}
        onClick={() => props.onChange(props.value.slice(0, -1))}
      >
        ⌫
      </button>
    </div>
  );
}
