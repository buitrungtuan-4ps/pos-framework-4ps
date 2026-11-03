// The operator UI's shared component kit, built only from the design tokens — the edge counterpart of
// the dashboard's `components/ui.tsx` (WS-E). Components carry no user-visible text of their own:
// every label is passed in already translated by the caller, so the no-hardcoded-strings lint
// (ADR-0020) has nothing to flag here.

import type { JSX } from "solid-js";

/**
 * A screen's top heading.
 *
 * `size` defaults to `lg` (the arm's-length operator screens); the KDS and expo screens pass `xl`
 * because they are read from two metres on a dark panel (`docs/ui-ux.md`). Keeping the size a prop
 * preserves that deliberate per-device difference while removing the duplicated `<h1>` markup every
 * screen was hand-rolling.
 */
export function PageHeader(props: { title: string; size?: "lg" | "xl" }): JSX.Element {
  const sizeClass = () => (props.size === "xl" ? "text-xl" : "text-lg");
  return <h1 class={`mb-4 ${sizeClass()} font-semibold`}>{props.title}</h1>;
}

/**
 * One row of a bill's arithmetic: a label on the left and its figure on the right, muted, as the
 * receipt prints it. `amount` is already formatted; `minus` marks a figure taken off.
 */
export function CheckRow(props: { label: string; amount: string; minus?: boolean; outcome?: string }): JSX.Element {
  return (
    <p class="flex justify-between text-sm text-ink-muted" data-outcome={props.outcome}>
      <span>{props.label}</span>
      <span class="tabular-nums">
        {props.minus ? "− " : ""}
        {props.amount}
      </span>
    </p>
  );
}
