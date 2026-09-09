// One read's own outcome, so a screen made of several reads is not all-or-nothing.
//
// A screen that assembles four independent requests must not blank on one failure, and must not
// render "nothing" and "we could not look" as the same thing. A page-level error banner makes "the
// alert service is down" and "this shop has no revenue yet" look identical, which is the mistake
// this type exists to prevent: each panel carries its own state, and a card that failed says so
// while its neighbours answer.
//
// Extracted from `screens/StoreHub.tsx`, which shaped it and still uses it, when the get-started
// checklist needed the same three states over seven reads.

import { ApiError } from "../api/client";

export type Panel<T> = { readonly state: "loading" } | { readonly state: "ready"; readonly value: T } | {
  readonly state: "failed";
  readonly message: string;
};

export const LOADING = { state: "loading" } as const;

/** Settles `promise` into `set` — the value on success, the refusal's own message on failure. */
export function panelOf<T>(promise: Promise<T>, set: (panel: Panel<T>) => void): Promise<void> {
  return promise.then(
    (value) => set({ state: "ready", value }),
    (caught: unknown) =>
      set({
        state: "failed",
        message: caught instanceof ApiError ? caught.message : String(caught),
      }),
  );
}
