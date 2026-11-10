// The device class the till draws for (ADR-0201 decision 3). It is the device dimension a package
// screen's variants are chosen by (ADR-0189 decision 4).
//
// The class the store publishes for this device comes first: the `device_class` the `TERMINAL`
// entry this device is bound to names, which `GET /api/session` answers (`../state/session.ts`).
// Where it names none, "read it from the screen" through the named breakpoints of
// `packages/shared/styles/tokens.css`, `terminal` from 64rem and `tablet` from 48rem, the phone
// below, which changes only when the screen crosses a breakpoint.

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
const [published, setPublished] = createSignal<string | undefined>(undefined);

if (typeof matchMedia === "function") {
  for (const breakpoint of BREAKPOINTS) {
    matchMedia(breakpoint.query).addEventListener("change", () => setCurrent(read()));
  }
}

/**
 * Takes the class the store publishes for this device, or none. `DEVICE_CLASS_UNSPECIFIED`, like
 * an edge too old to send one, is none: the screen says.
 */
export function publishDeviceClass(deviceClass: string | undefined): void {
  setPublished(deviceClass === "DEVICE_CLASS_UNSPECIFIED" ? undefined : deviceClass);
}

/** The device class the till draws for now. */
export function deviceClass(): string {
  return published() ?? current();
}
