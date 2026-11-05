// A till screen drawn from the default package (ADR-0190): the screens this release ships, as a
// package, in `default-package.json`. The cloud and the edge validate a published package with
// `pos_core::package::check_screens`, and a `pos-core` test runs the same check on this file, so the
// default package cannot hold a screen a published one could not.
//
// A store pinned to a package version still draws the default package: the edge does not serve a
// version's screens to the till until the console can author them (phase 4).

import type { JSX } from "solid-js";

import type { Screen } from "./components";
import DEFAULT_PACKAGE from "./default-package.json";
import { IMPLEMENTATIONS } from "./registry";
import { NeedsUpdate, PackageTree, type ScreenHost } from "./render";
import { resolveVariant, type Variant, type VariantContext } from "./variant";

/** The default package's screens, each with its variants. */
const SCREENS: readonly Variant<Screen>[] = DEFAULT_PACKAGE.screens;

/** The variant of `screenId` this device draws, if the default package has the screen. */
export function defaultScreen(screenId: string, context: VariantContext = {}): Screen | undefined {
  return resolveVariant(
    SCREENS.filter((variant) => variant.body.screen_id === screenId),
    context,
  );
}

/** A host for a screen whose components bind no act. */
export const NO_ACTS: ScreenHost = {
  subject: () => ({}),
  offers: () => false,
  run: () => undefined,
};

/**
 * The screen `screenId` of the default package, drawn for `host`. A screen the package does not
 * hold draws the slot that says the till needs an update.
 */
export function PackageScreen(props: { screenId: string; host: ScreenHost }): JSX.Element {
  const screen = defaultScreen(props.screenId);
  if (screen === undefined) {
    return <NeedsUpdate />;
  }
  return <PackageTree root={screen.root} host={props.host} implementations={IMPLEMENTATIONS} fallback={<NeedsUpdate />} />;
}
