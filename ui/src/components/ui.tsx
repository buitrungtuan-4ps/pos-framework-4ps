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
