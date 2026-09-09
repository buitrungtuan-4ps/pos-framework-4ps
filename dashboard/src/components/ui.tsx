// The dashboard's small component kit, built only from the design tokens (ADR-0060 reuses the P6
// token set): one radius, one border, the token colour roles, the 48px touch minimum. None of these
// carry user-visible text of their own — every label, placeholder and message is passed in already
// translated by the caller — so the no-hardcoded-strings lint (ADR-0020) has nothing to flag here.

import type { JSX, ParentProps } from "solid-js";
import { createUniqueId, For, Show, splitProps } from "solid-js";

import { locale } from "../i18n";

/** A titled panel. `title` is already-translated text; `actions` sits on the header's right. */
export function Card(props: ParentProps<{ title: string; actions?: JSX.Element }>) {
  return (
    <section class="rounded-token border border-line bg-surface shadow-raised">
      <header class="flex items-center justify-between gap-4 border-b border-line px-4 py-3">
        <h2 class="text-lg font-semibold text-ink">{props.title}</h2>
        <Show when={props.actions}>{props.actions}</Show>
      </header>
      <div class="p-4">{props.children}</div>
    </section>
  );
}

/** A coloured status pill. `label` is already-translated text. `danger` is for an active fault
 *  (a firing alert, a critical severity): `text-danger` clears AA on the card surface, and the label
 *  always rides with the hue, so meaning is never carried by colour alone.
 *
 *  A primitive rather than part of the CRUD kit, and moved here on a measurement: the account
 *  menu in the header wanted one badge, and importing it from `kit.tsx` merged that whole lazy
 *  chunk into the shell bundle — 14 kB added to every first visit for a coloured pill. It has no
 *  dependencies and no state, which is what makes `ui.tsx` its home. */
export function StatusBadge(props: {
  label: string;
  tone: "active" | "archived" | "disabled" | "neutral" | "danger";
}) {
  const palette = () => {
    switch (props.tone) {
      case "active":
        return "border-ok text-ok";
      case "danger":
        return "border-danger text-danger";
      case "archived":
      case "disabled":
        return "border-ink-muted text-ink-muted";
      default:
        return "border-line text-ink-muted";
    }
  };
  return (
    <span
      class={`inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium ${palette()}`}
    >
      {props.label}
    </span>
  );
}

/** The page title and optional one-line description at the top of every screen. */
export function PageHeader(props: { title: string; description?: string }) {
  return (
    <div class="mb-6">
      <h1 class="text-xl font-semibold text-ink">{props.title}</h1>
      <Show when={props.description}>
        <p class="mt-1 text-sm text-ink-muted">{props.description}</p>
      </Show>
    </div>
  );
}

type ButtonProps = JSX.ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "primary" | "secondary" | "danger";
};

/** A button sized to the 48px touch minimum, coloured by role. Content (the label) is a child. */
export function Button(props: ButtonProps) {
  const [local, rest] = splitProps(props, ["variant", "class", "children"]);
  const palette = () => {
    switch (local.variant) {
      case "danger":
        return "bg-danger text-danger-ink";
      case "secondary":
        return "bg-surface-raised text-ink border border-line";
      default:
        return "bg-accent text-accent-ink";
    }
  };
  return (
    <button
      {...rest}
      class={`inline-flex min-h-touch items-center justify-center rounded-token px-4 text-base font-medium transition-[filter] hover:brightness-95 disabled:cursor-not-allowed disabled:opacity-50 ${palette()} ${local.class ?? ""}`}
    >
      {local.children}
    </button>
  );
}

/**
 * A labelled single-line input. `label`/`placeholder`/`hint` are already-translated text.
 *
 * `hint` is standing guidance about the field — what it accepts, or that it may be left blank — not
 * a validation error, which is `FormField`'s job. It renders below the input and is wired with
 * `aria-describedby`, so a screen reader announces it *after* the label rather than instead of it;
 * a `placeholder` cannot do that (it disappears on the first keystroke, which is exactly when a
 * "you can leave this blank" hint still matters).
 */
export function TextField(
  props: {
    label: string;
    value: string;
    onInput: (value: string) => void;
    hint?: string;
  } & Pick<JSX.InputHTMLAttributes<HTMLInputElement>, "type" | "placeholder" | "autocomplete">,
) {
  const hintId = createUniqueId();
  return (
    <label class="block">
      <span class="mb-1 block text-sm font-medium text-ink">{props.label}</span>
      <input
        class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
        type={props.type ?? "text"}
        placeholder={props.placeholder}
        autocomplete={props.autocomplete}
        aria-describedby={props.hint ? hintId : undefined}
        value={props.value}
        onInput={(event) => props.onInput(event.currentTarget.value)}
      />
      <Show when={props.hint}>
        {(hint) => (
          <span id={hintId} class="mt-1 block text-sm text-ink-muted">
            {hint()}
          </span>
        )}
      </Show>
    </label>
  );
}

/**
 * A labelled `<select>`, the sibling `TextField` never had
 * ([ADR-0121](../../../docs/adr/0121-one-way-to-author-an-entity.md) §5).
 *
 * Its absence is why there are 53 raw `<select>` across 22 screens. That is not a styling
 * complaint: each hand-rolled one re-invents the label association — most wrap the control in a
 * bare `<label>` with a `<span>`, which works, but nothing checks it — and none can be reached by
 * `FormField`'s error slot, so a refused choice has nowhere to say so.
 *
 * `options` carries already-translated labels; the empty-value option is `placeholder`, present only
 * when given, so "no brand" is an explicit choice a caller opts into rather than a blank row every
 * select inherits.
 */
export function SelectField(props: {
  label: string;
  value: string;
  options: readonly { readonly value: string; readonly label: string }[];
  onChange: (value: string) => void;
  placeholder?: string;
  hint?: string;
  disabled?: boolean;
}) {
  const hintId = createUniqueId();
  return (
    <label class="block">
      <span class="mb-1 block text-sm font-medium text-ink">{props.label}</span>
      <select
        class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink disabled:cursor-not-allowed disabled:opacity-50"
        value={props.value}
        disabled={props.disabled}
        aria-describedby={props.hint ? hintId : undefined}
        onChange={(event) => props.onChange(event.currentTarget.value)}
      >
        <Show when={props.placeholder}>
          {(placeholder) => <option value="">{placeholder()}</option>}
        </Show>
        <For each={props.options}>
          {(option) => <option value={option.value}>{option.label}</option>}
        </For>
      </select>
      <Show when={props.hint}>
        {(hint) => (
          <span id={hintId} class="mt-1 block text-sm text-ink-muted">
            {hint()}
          </span>
        )}
      </Show>
    </label>
  );
}

/**
 * A money input (ADR-0082) that edits an integer amount in a currency's smallest unit — the exact
 * `amount_minor` it stores — grouping the digits for the active locale as the operator types and
 * showing the (separately chosen) currency code as a static adornment. Only digits are accepted; an
 * empty field emits `null` (not priced). It carries no currency conversion or fractional handling —
 * VND (v1) has no minor part, and other currencies are authored in their minor units, the same
 * convention `formatMoney` reads back.
 */
export function MoneyField(props: {
  label: string;
  currencyCode: string;
  value: number | null;
  onChange: (minor: number | null) => void;
  placeholder?: string;
}) {
  const grouped = () =>
    props.value === null ? "" : new Intl.NumberFormat(locale()).format(props.value);
  return (
    <label class="block">
      <span class="mb-1 block text-sm font-medium text-ink">{props.label}</span>
      <div class="flex items-center gap-2">
        <input
          class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
          inputmode="numeric"
          placeholder={props.placeholder}
          value={grouped()}
          onInput={(event) => {
            const digits = event.currentTarget.value.replace(/\D/g, "");
            props.onChange(digits === "" ? null : Number(digits));
          }}
        />
        <span class="shrink-0 text-sm text-ink-muted">{props.currencyCode}</span>
      </div>
    </label>
  );
}

/** A labelled multi-line input, for JSON documents and translation values. */
export function TextArea(props: {
  label: string;
  value: string;
  onInput: (value: string) => void;
  rows?: number;
  placeholder?: string;
}) {
  return (
    <label class="block">
      <span class="mb-1 block text-sm font-medium text-ink">{props.label}</span>
      <textarea
        class="w-full rounded-token border border-line bg-surface-raised p-3 font-mono text-sm text-ink"
        rows={props.rows ?? 10}
        placeholder={props.placeholder}
        value={props.value}
        onInput={(event) => props.onInput(event.currentTarget.value)}
      />
    </label>
  );
}

/**
 * A placeholder in roughly the shape of the content that is loading.
 *
 * # Why a shape instead of the word "Loading…"
 *
 * Fourteen places in this console answered a pending read with `<p>Loading…</p>` — a line of text
 * where a table, a card or a whole screen is about to appear. The page therefore jumps twice: once
 * when the sentence replaces nothing, and again when the real content replaces the sentence and
 * pushes everything below it. A block the size of the answer removes the second jump, and it tells
 * an operator *what* is coming, not merely that something is.
 *
 * This is perceived performance and it is worth being precise about that: the console has no
 * measured load problem in the path an operator uses. What it had was a load *appearance* problem.
 *
 * # The accessibility half, which is the half easy to get wrong
 *
 * Replacing the sentence with bare grey bars would make the loading state **silent** for a screen
 * reader — a regression dressed as an improvement. So the container is a `status` region carrying
 * the same words the sentence carried, and the bars are `aria-hidden`: a sighted reader gets the
 * shape, a screen-reader user keeps the announcement, and neither loses anything.
 *
 * The pulse needs no `prefers-reduced-motion` guard of its own — `app.css` already clamps every
 * animation to 0.01ms under that preference, for exactly this class of decoration.
 *
 * `label` is already-translated text, like every other string in this file.
 */
export function Skeleton(props: { label: string; rows?: number; class?: string }) {
  const rows = () => Math.max(1, props.rows ?? 3);
  return (
    <div role="status" aria-label={props.label} class={props.class}>
      <div class="flex flex-col gap-2" aria-hidden="true">
        <For each={Array.from({ length: rows() }, (_, position) => position)}>
          {(position) => (
            // `bg-line` rather than `bg-surface-raised`: raised is a hair off the surface it sits
            // on (0.995 against 1.0 in the light theme) and a bar in it is invisible. The last bar
            // is short, because the last line of real content usually is — an even stack of full
            // bars reads as a loading widget, an uneven one reads as content arriving.
            <div
              class={`h-4 animate-pulse rounded-token bg-line ${
                position === rows() - 1 && rows() > 1 ? "w-2/3" : "w-full"
              }`}
            />
          )}
        </For>
      </div>
    </div>
  );
}

/** A dismissable status line. `tone` sets the colour; `message` is already-translated text. */
export function Banner(props: { tone: "ok" | "danger"; message: string }) {
  const palette = props.tone === "ok" ? "border-ok text-ok" : "border-danger text-danger";
  return (
    <div
      role={props.tone === "danger" ? "alert" : "status"}
      class={`rounded-token border bg-surface-raised px-3 py-2 text-sm ${palette}`}
    >
      {props.message}
    </div>
  );
}
