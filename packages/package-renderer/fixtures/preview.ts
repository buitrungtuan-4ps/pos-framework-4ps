// The one fixture both front ends draw (ADR-0209 decision 4): the default package's screens and a
// designed screen with a variant for phones (`preview-package.json`), drawn in fixed words for a
// fixed host, so the shape depends on the renderer and the front end's table alone. The till's
// `unit/preview-matches.test.ts` and the console's `tests/preview-matches.test.ts` each write the
// shape and compare it with `preview-shape.txt` (Vitest's `toMatchFileSnapshot`; `-u` on the till's
// test rewrites it).

import type { Screen, ScreenNode } from "../src/components";
import type { ScreenHost, Words } from "../src/render";
import { shape } from "../src/shape";
import type { Variant } from "../src/variant";
import FIXTURE from "./preview-package.json";

/** The fixture's screens, each with its variants. */
export const FIXTURE_SCREENS = FIXTURE.screens as unknown as readonly Variant<Screen>[];

/** Words that show a key as itself, so the shape names what each node was given. */
export const FIXTURE_WORDS: Words = {
  label: (key) => `«${key}»`,
  needsUpdate: () => "«needs an update»",
  failure: () => "«failed»",
};

/** A host that offers opening a screen and nothing else, and runs nothing. */
export const FIXTURE_HOST: ScreenHost = {
  subject: () => ({}),
  offers: (actionId) => actionId === "ui.screen.open",
  run: () => undefined,
};

/** The committed shape, as a test in `ui/unit/` or `dashboard/tests/` names it. */
export const SHAPE_FILE = "../../packages/package-renderer/fixtures/preview-shape.txt";

/** Every variant's shape, as `draw` mounts its tree into an element and returns it. */
export function shapes(draw: (root: ScreenNode) => Element): string {
  return `${FIXTURE_SCREENS.map((variant) => {
    const deviceClass = variant.selector?.device_class ?? "every device";
    return `== ${variant.body.screen_id} · ${deviceClass}\n${shape(draw(variant.body.root))}`;
  }).join("\n")}\n`;
}
