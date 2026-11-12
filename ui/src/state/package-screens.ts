// The running package version's screens (ADR-0190 decision 3): what `GET /api/package` answers as
// `running.screens`, every variant of each in the version's order. A screen the version declares is
// drawn in place of the default package's screen of the same id; any other is the default
// package's, as is every screen of a store on the default package (ADR-0189 decision 7).
//
// The answer is kept only when it changed, so a refetch that brings the same screens back leaves
// every drawn tree as it is.

import { createSignal } from "solid-js";

import type { Screen } from "../package/components";
import type { Variant } from "../package/variant";

const [published, setPublished] = createSignal<readonly Variant<Screen>[]>([]);
let kept = "[]";

/** Takes the screens a `GET /api/package` answer carries, or none. */
export function applyScreens(body: unknown): void {
  const running = (body as { running?: { screens?: Variant<Screen>[] } | null } | null)?.running;
  const screens = running?.screens ?? [];
  const text = JSON.stringify(screens);
  if (text !== kept) {
    kept = text;
    setPublished(screens);
  }
}

/** The running version's variants of `screenId`, none when it does not declare the screen. */
export function publishedScreen(screenId: string): readonly Variant<Screen>[] {
  return published().filter((variant) => variant.body.screen_id === screenId);
}
