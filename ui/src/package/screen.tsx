// A till screen drawn from its package (ADR-0190): the running version's screen of that id where
// the version declares one (`../state/package-screens.ts`), else the default package's, the
// screens this release ships in `default-package.json`. The cloud and the edge validate a published
// package with `pos_core::package::check_screens`, and a `pos-core` test runs the same check on
// this file, so the default package cannot hold a screen a published one could not.
//
// The variant drawn is the one for this device's class (`../lib/device-class.ts`) and the signed-in
// person's roles (`../state/session.ts`, ADR-0213). A new tree is
// mounted only when the variant chosen changes: a new version, or the screen crossing a breakpoint
// into a class the screen has its own variant for.

import { createMemo, Show, type JSX } from "solid-js";

import { deviceClass } from "../lib/device-class";
import { DRAFT } from "../state/draft-walk-in";
import { LOOKUP_ACT, lookupRuns, runLookup } from "../state/lookup";
import { publishedScreen } from "../state/package-screens";
import { buttonRuns, runButton } from "../state/reactions";
import { roleIds } from "../state/session";
import type { Screen } from "../../../packages/package-renderer/src/components";
import { PackageTree, type ScreenHost, type Supplied } from "../../../packages/package-renderer/src/render";
import { resolveVariant, type Variant, type VariantContext } from "../../../packages/package-renderer/src/variant";
import { PENDING, state } from "../state/store";

import DEFAULT_PACKAGE from "./default-package.json";
import { IMPLEMENTATIONS } from "./registry";
import { NeedsUpdate, TILL_WORDS } from "./words";

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
 * A host for a screen whose own components bind no act, for the order and table `subject` gives:
 * a package's button there runs what a reaction may run (`../state/reactions.ts`), for that
 * subject, when the signed-in person holds it outright. That is opening a screen and the prints,
 * never an act that moves money; and the screen's lookup (`../state/lookup.ts`, ADR-0214), which
 * only reads. Any other act it binds draws disabled.
 */
export function screenHost(subject: () => Supplied): ScreenHost {
  return {
    subject,
    offers: (actionId) => (actionId === LOOKUP_ACT ? lookupRuns() : buttonRuns(actionId)),
    run: (binding, supplied) => (binding.action_id === LOOKUP_ACT ? runLookup(binding, supplied) : runButton(binding, supplied)),
  };
}

/** The host of a screen that is for no order or table. */
export const NO_ACTS: ScreenHost = screenHost(() => ({}));

/**
 * The order screen's host (ADR-0203 decision 3): a table's screen supplies its `table_id` and,
 * once the edge has opened it, its order's `order_id`; a walk-in's screen its `order_id`, once it
 * is more than a draft. Read on the tap, so a button acts on the order shown then.
 */
export function orderHost(walkIn: () => boolean, shown: { readonly id: string }): ScreenHost {
  return screenHost((): Supplied => {
    if (walkIn()) {
      return shown.id === DRAFT ? {} : { order_id: shown.id };
    }
    const orderId = state.tableOrder[shown.id];
    return orderId === undefined || orderId.startsWith(PENDING) ? { table_id: shown.id } : { table_id: shown.id, order_id: orderId };
  });
}

/**
 * The screen `screenId` of the package, drawn for `host`. A screen neither the running version nor
 * the default package holds draws the slot that says the till needs an update.
 */
export function PackageScreen(props: { screenId: string; host: ScreenHost }): JSX.Element {
  // The person's roles choose a role variant (ADR-0213 decision 3).
  const screen = createMemo(() => packageScreen(props.screenId, { device_class: deviceClass(), role_ids: roleIds() }));
  return (
    <Show when={screen()} keyed fallback={<NeedsUpdate />}>
      {(drawn) => <PackageTree root={drawn.root} host={props.host} implementations={IMPLEMENTATIONS} words={TILL_WORDS} fallback={<NeedsUpdate />} />}
    </Show>
  );
}
