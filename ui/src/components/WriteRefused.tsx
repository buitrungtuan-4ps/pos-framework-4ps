// An edge write the edge refused, or did not answer (ADR-0196 decision 1): its paint is already put
// back, and this says why, in place above what was tapped, until the next tap of the same act. Not
// for money, whose refusal must be dismissed (`RefusalPanel`).

import { Show } from "solid-js";

export function WriteRefused(props: { readonly message: string | null; readonly outcome: string }) {
  return (
    <Show when={props.message}>
      {(message) => (
        <p class="mb-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert" data-outcome={props.outcome}>
          {message()}
        </p>
      )}
    </Show>
  );
}
