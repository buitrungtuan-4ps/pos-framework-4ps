// A till screen drawn from its package (ADR-0190): the running version's screen of that id where
// the version declares one (`../state/package-screens.ts`), else the default package's, the
// screens this release ships in `default-package.json`. The cloud and the edge validate a published
// package with `pos_core::package::check_screens`, and a `pos-core` test runs the same check on
// this file, so the default package cannot hold a screen a published one could not.
//
// The variant drawn is the one for this device's class (`../lib/device-class.ts`). A new tree is
// mounted only when the variant chosen changes: a new version, or the screen crossing a breakpoint
// into a class the screen has its own variant for.

import { createMemo, Show, type JSX } from "solid-js";

import { deviceClass } from "../lib/device-class";
import { publishedScreen } from "../state/package-screens";
import { openPackageScreen } from "../state/reactions";

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

/**
 * The variant of `screenId` this device draws: the running version's, if it declares the screen,
 * else the default package's.
 */
export function packageScreen(screenId: string, context: VariantContext = {}): Screen | undefined {
  const own = publishedScreen(screenId);
  return own.length > 0 ? resolveVariant(own, context) : defaultScreen(screenId, context);
}

/**
 * A host for a screen whose own components bind no act: a package's button there may open another
 * screen that needs no table (`ui.screen.open`), and any other act it binds draws disabled.
 */
export const NO_ACTS: ScreenHost = {
  subject: () => ({}),
  offers: (actionId) => actionId === "ui.screen.open",
  run: (binding) => {
    const screenId = binding.params?.["screen_id"];
    if (binding.action_id === "ui.screen.open" && typeof screenId === "string") {
      openPackageScreen(screenId);
    }
  },
};

/**
 * The screen `screenId` of the package, drawn for `host`. A screen neither the running version nor
 * the default package holds draws the slot that says the till needs an update.
 */
export function PackageScreen(props: { screenId: string; host: ScreenHost }): JSX.Element {
  const screen = createMemo(() => packageScreen(props.screenId, { device_class: deviceClass() }));
  return (
    <Show when={screen()} keyed fallback={<NeedsUpdate />}>
      {(drawn) => <PackageTree root={drawn.root} host={props.host} implementations={IMPLEMENTATIONS} fallback={<NeedsUpdate />} />}
    </Show>
  );
}
