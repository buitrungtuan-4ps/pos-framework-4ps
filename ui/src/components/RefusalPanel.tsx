// A money act the edge refused, or the till refused before sending it (ADR-0196 safeguard 3): the
// refusal's own sentence, in place where the act was tapped, which the operator must dismiss before
// the act's controls take a tap again. In place rather than over the screen, as `SendFirst` is:
// `docs/ui-ux.md` §1 allows nothing that covers the screen. What the operator typed stays.

import { Show } from "solid-js";

import { t } from "../i18n";

export function RefusalPanel(props: { readonly message: string | null; readonly onDismiss: () => void }) {
  return (
    <Show when={props.message}>
      {(message) => (
        <div
          class="mt-3 rounded-token border border-danger bg-surface p-3"
          role="alertdialog"
          aria-labelledby="refusal-message"
          data-outcome="money-refused"
        >
          <p id="refusal-message" class="text-danger">
            {message()}
          </p>
          <button
            type="button"
            class="mt-3 min-h-touch w-full rounded-token border border-line font-semibold"
            data-step="dismissRefusal"
            onClick={() => props.onDismiss()}
          >
            {t("common.dismiss")}
          </button>
        </div>
      )}
    </Show>
  );
}
