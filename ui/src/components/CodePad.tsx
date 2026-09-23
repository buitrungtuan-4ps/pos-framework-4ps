// The on-screen keyboard a till needs to let somebody start their shift.
//
// `docs/ui-ux.md` §85 has asked for "a shared component for numeric and text entry on touch
// devices without a physical keyboard" since it was written. `Keypad` built the numeric half, for
// cash. This is the other half, and the screen that needed it most had nothing: **sign-in**.
//
// The gap is not cosmetic and it is not the PIN. `SignIn`'s PIN field already strips non-digits, so
// a numeric pad serves it. The badge code does not: it is free text, and the console's own
// placeholder for it reads `e.g. A01` — a letter, then digits. On a fixed terminal whose
// on-screen keyboard is switched off, `inputmode` asks the platform for a keyboard that never
// comes and the field cannot be filled. That is not one awkward field; it is a till nobody can
// sign in to, which is every field on every later screen.
//
// Why it is not `Keypad` with a flag. That component drops a leading zero — `0` then `5` is five
// — which is right for money, where `parseWhole` would otherwise read "05", and wrong here twice
// over: `0512` is a plausible PIN and `0012` a plausible badge code. Collapsing the two would mean
// two flags to express one difference, and would put a money rule inside a credential control.
//
// Letters in alphabetical order rather than QWERTY. A badge code is three or four characters hunted
// one at a time, not prose typed by touch; A–Z is scannable, and somebody who has never used a
// keyboard finds a letter in it faster.
//
// 48px keys, the floor `docs/ui-ux.md` §2 sets for a touch target, rather than the 56px the cash
// keypad uses. That one is pressed repeatedly and under time pressure; this one is pressed once at
// the start of a shift.
//
// It does not replace the field it sits under, exactly as `Keypad` does not. A terminal with a USB
// keyboard and a phone with an OS keyboard both still type into that field, and taking it away
// would trade one device class's problem for another's.

import { For, Show } from "solid-js";

import { t } from "../i18n";

/** A–Z, in the order somebody hunting for one letter expects to find it. */
const LETTERS = "ABCDEFGHIJKLMNOPQRSTUVWXYZ".split("");

/** `0`–`9`, in keyboard-row order rather than the calculator order money uses. */
const DIGITS = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"];

export function CodePad(props: {
  /** What has been typed so far. */
  value: string;
  /** Called with the next value; the caller owns the state, as it owns the field. */
  onChange: (next: string) => void;
  /**
   * `text` draws the letters as well; `digits` draws the digits alone.
   *
   * Driven by which field is being filled rather than by a preference, so a letter cannot be typed
   * into a field that would only strip it again and leave the operator wondering what they did.
   */
  mode: "text" | "digits";
  /** Names the pad for a screen reader; a grid of single characters says nothing on its own. */
  label: string;
  /** The container's handle, so a harness can address one pad on a screen that could hold two. */
  id?: string;
}) {
  // No leading-zero rule: every character pressed is kept, because a credential is a string and not
  // a quantity. See the header.
  const press = (character: string) => props.onChange(props.value + character);

  const key =
    "min-h-touch rounded-token border border-line bg-surface-raised text-lg font-semibold text-ink active:bg-surface";

  // Pressing a key must not take the caret out of the field it is filling. `pointerdown` is where a
  // button steals focus, so that is where it is refused — `click` still fires and still types.
  const hold = (event: PointerEvent) => event.preventDefault();

  return (
    <div id={props.id} class="mt-3 flex flex-col gap-2" role="group" aria-label={props.label}>
      <Show when={props.mode === "text"}>
        <div class="grid grid-cols-6 gap-2">
          <For each={LETTERS}>
            {(letter) => (
              <button
                type="button"
                class={key}
                onPointerDown={hold}
                onClick={() => press(letter)}
              >
                {letter}
              </button>
            )}
          </For>
        </div>
      </Show>

      <div class="grid grid-cols-5 gap-2">
        <For each={DIGITS}>
          {(digit) => (
            <button
              type="button"
              class={key}
              onPointerDown={hold}
              onClick={() => press(digit)}
            >
              {digit}
            </button>
          )}
        </For>
      </div>

      <div class="grid grid-cols-2 gap-2">
        <button
          type="button"
          class={key}
          onPointerDown={hold}
          aria-label={t("codepad.clear")}
          onClick={() => props.onChange("")}
        >
          {t("codepad.clear_short")}
        </button>
        <button
          type="button"
          class={key}
          onPointerDown={hold}
          aria-label={t("codepad.backspace")}
          onClick={() => props.onChange(props.value.slice(0, -1))}
        >
          ⌫
        </button>
      </div>
    </div>
  );
}
