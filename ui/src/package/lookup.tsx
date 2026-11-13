// A package screen's lookup, as the till draws it (ADR-0214): `lookup_field`, a box a person types
// one of the lookup's values into, and `lookup_result`, one field of the answer under its label. The
// screen's `action_button` for `integrations.lookup.run` sends it (`./screen.tsx`); what was typed
// and the answer are `../state/lookup.ts`'s, held for the screen only.
//
// Each drops what was typed and the answer when it goes, so they last as long as the screen that
// draws them (ADR-0214 decision 4). Function declarations, never `const`, as every implementation
// in `./registry.tsx`.

import { createSignal, onCleanup, Show, type JSX } from "solid-js";

import { textProperty, useWords, type NodeProps } from "../../../packages/package-renderer/src/render";
import { t } from "../i18n";
import { forgetLookup, lookupAnswer, sendSubjectField, SUBJECT_FIELDS, typeLookupValue } from "../state/lookup";

/** A whole number as typed, or the text itself, which the edge refuses as the wrong kind. */
function wholeNumber(text: string): number | string {
  const number = /^-?\d{1,15}$/.test(text) ? Number(text) : Number.NaN;
  return Number.isSafeInteger(number) ? number : text;
}

/** A box for one value of the screen's lookup. One naming `order_id` or `table_id` draws nothing. */
export function LookupField(props: NodeProps): JSX.Element {
  const words = useWords();
  onCleanup(forgetLookup);
  const field = textProperty(props.node, "field") ?? "";
  if (SUBJECT_FIELDS.has(field)) {
    sendSubjectField(field);
    return null;
  }
  const number = textProperty(props.node, "kind") === "number";
  const [text, setText] = createSignal("");
  const input = (raw: string) => {
    setText(raw);
    const trimmed = raw.trim();
    typeLookupValue(field, trimmed === "" ? undefined : number ? wholeNumber(trimmed) : trimmed);
  };
  return (
    <label class="block text-sm font-medium">
      {words.label(textProperty(props.node, "label_key") ?? "")}
      <input
        type="text"
        inputMode={number ? "numeric" : "text"}
        autocomplete="off"
        class="mt-1 block min-h-touch w-full rounded-token border border-line bg-surface px-3 text-base"
        data-lookup-field={field}
        value={text()}
        onInput={(event) => input(event.currentTarget.value)}
      />
    </label>
  );
}

/** One field of the answer, as the till shows a value an endpoint declares: text, a number, yes or no. */
function shown(value: unknown): string {
  if (value === undefined || value === null) {
    return t("lookup.none");
  }
  if (typeof value === "boolean") {
    return t(value ? "lookup.yes" : "lookup.no");
  }
  return typeof value === "string" || typeof value === "number" ? String(value) : t("lookup.none");
}

/** One field of the screen's last answer under its label; while the lookup runs, that it does. */
export function LookupResult(props: NodeProps): JSX.Element {
  const words = useWords();
  onCleanup(forgetLookup);
  const field = () => textProperty(props.node, "field") ?? "";
  const text = () => {
    const answer = lookupAnswer();
    if (answer.state === "pending") {
      return t("lookup.pending");
    }
    return answer.state === "answered" ? shown(answer.result[field()]) : undefined;
  };
  return (
    <Show when={text()}>
      {(value) => (
        <p class="flex justify-between gap-3" data-lookup-result={field()} aria-live="polite">
          <span class="text-ink-muted">{words.label(textProperty(props.node, "label_key") ?? "")}</span>
          <span class="font-semibold">{value()}</span>
        </p>
      )}
    </Show>
  );
}
