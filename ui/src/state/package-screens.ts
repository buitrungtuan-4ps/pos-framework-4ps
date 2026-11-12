// The running package version's screens (ADR-0190 decision 3): what `GET /api/package` answers as
// `running.screens`, every variant of each in the version's order. A screen the version declares is
// drawn in place of the default package's screen of the same id; any other is the default
// package's, as is every screen of a store on the default package (ADR-0189 decision 7).
//
// The answer is kept only when it changed, so a refetch that brings the same screens back leaves
// every drawn tree as it is.

import { createSignal } from "solid-js";

import type { Screen } from "../../../packages/package-renderer/src/components";
import { resolveVariant, type Variant, type VariantContext } from "../../../packages/package-renderer/src/variant";

/** A role's home: the screen its tills open after sign-in (ADR-0210 decision 1). */
export interface HomeScreen {
  screen_id: string;
}

const [published, setPublished] = createSignal<readonly Variant<Screen>[]>([]);
let kept = "[]";
let homes: readonly Variant<HomeScreen>[] = [];

/** Takes the screens and the homes a `GET /api/package` answer carries, or none. */
export function applyScreens(body: unknown): void {
  const running = (body as { running?: { screens?: Variant<Screen>[]; home_screens?: Variant<HomeScreen>[] } | null } | null)?.running;
  homes = running?.home_screens ?? [];
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

/**
 * The screen the running version names as home for `context` (ADR-0210 decision 1), none when it
 * states no homes: the till then starts where its release starts it.
 */
export function homeScreenId(context: VariantContext): string | undefined {
  return resolveVariant(homes, context)?.screen_id;
}
