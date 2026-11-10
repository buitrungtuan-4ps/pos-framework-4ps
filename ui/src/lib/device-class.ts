// The device class the till draws for (ADR-0201 decision 3): until the `devices` node names one,
// "read it from the screen" through the named breakpoints of `packages/shared/styles/tokens.css`,
// `terminal` from 64rem and `tablet` from 48rem, the phone below. It is the device dimension a
// package screen's variants are chosen by (ADR-0189 decision 4), and it changes only when the
// screen crosses a breakpoint.

import { createSignal } from "solid-js";

type Watched = { readonly query: string; readonly deviceClass: string };

const BREAKPOINTS: readonly Watched[] = [
  { query: "(min-width: 64rem)", deviceClass: "DEVICE_CLASS_TERMINAL" },
  { query: "(min-width: 48rem)", deviceClass: "DEVICE_CLASS_TABLET" },
];

function read(): string {
  if (typeof matchMedia !== "function") {
    return "DEVICE_CLASS_TERMINAL";
  }
  return BREAKPOINTS.find((breakpoint) => matchMedia(breakpoint.query).matches)?.deviceClass ?? "DEVICE_CLASS_PHONE";
}

const [current, setCurrent] = createSignal(read());

if (typeof matchMedia === "function") {
  for (const breakpoint of BREAKPOINTS) {
    matchMedia(breakpoint.query).addEventListener("change", () => setCurrent(read()));
  }
}

/** The device class the till's screen is now. */
export function deviceClass(): string {
  return current();
}
