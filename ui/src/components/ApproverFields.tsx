import { Show, createSignal } from "solid-js";

import { t } from "../i18n";
import { CodePad } from "./CodePad";

// A manager's badge code and PIN, typed in place on a panel that needs one — voiding a fired line,
// voiding a bill, taking money off — with the on-screen credential pad beneath them.
//
// The three panels each drew the two fields as plain inputs, which a till with no keyboard cannot
// fill: `inputmode` asks the platform for a keyboard, and a fixed terminal with its on-screen
// keyboard switched off has none to give (`docs/ui-ux.md` §2). Sign-in had the same gap and closed
// it with `CodePad`; the panels still had it, so on a POS Station or Terminal touch screen a void or
// a discount was impossible. Three copies of the same pair is the third occurrence, so they are one
// component now (`docs/design-principles.md`, rule of three).
//
// The pad appears once one of the fields has focus, and follows it: letters and digits for the
// badge, digits alone for the PIN — sign-in's rule. It does not appear before, because these panels
// already carry a reason picker (and, for a discount, the amount's own keypad), and a pad nobody has
// asked for pushes the reasons below the fold on a phone.
export function ApproverFields(props: {
  /** The badge code typed so far. */
  code: string;
  /** The PIN typed so far. */
  pin: string;
  onCode: (next: string) => void;
  onPin: (next: string) => void;
  /** The two field labels, which each panel words for its own act. */
  codeLabel: string;
  pinLabel: string;
  /** Prefix for the fields' and pad's ids, so a harness can address this panel's own. */
  id: string;
}) {
  const [filling, setFilling] = createSignal<"code" | "pin" | null>(null);

  // A PIN is digits, whichever way it was typed: the pad in digits mode cannot type a letter, and a
  // letter from a physical keyboard is dropped for the same reason sign-in drops it.
  const digitsOnly = (text: string) => text.replace(/\D/g, "");
  const padValue = () => (filling() === "pin" ? props.pin : props.code);
  const padChange = (next: string) => {
    if (filling() === "pin") {
      props.onPin(digitsOnly(next));
    } else {
      props.onCode(next);
    }
  };

  return (
    <>
      <label class="mt-2 block text-sm" for={`${props.id}-code`}>
        {props.codeLabel}
      </label>
      <input
        id={`${props.id}-code`}
        type="text"
        autocomplete="off"
        class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-2"
        value={props.code}
        onFocus={() => setFilling("code")}
        onInput={(event) => props.onCode(event.currentTarget.value)}
      />
      <label class="mt-2 block text-sm" for={`${props.id}-pin`}>
        {props.pinLabel}
      </label>
      <input
        id={`${props.id}-pin`}
        type="password"
        inputmode="numeric"
        autocomplete="off"
        class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-2"
        value={props.pin}
        onFocus={() => setFilling("pin")}
        onInput={(event) => props.onPin(digitsOnly(event.currentTarget.value))}
      />
      <Show when={filling()}>
        {(field) => (
          <CodePad
            id={`${props.id}-pad`}
            value={padValue()}
            onChange={padChange}
            mode={field() === "code" ? "text" : "digits"}
            label={field() === "code" ? t("approver.pad_code") : t("approver.pad_pin")}
          />
        )}
      </Show>
    </>
  );
}
