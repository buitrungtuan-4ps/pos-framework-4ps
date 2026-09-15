// The three things a `PublishBar` can say, in the console's words.
//
// The kit decides *which* of the three a node is in (`publishState`); this turns that into a
// sentence. One function rather than three copies per screen, because thirteen screens saying the
// same thing three slightly different ways is the finding this wave is closing (F13), and a screen
// that wants its own wording can still pass its own `describe`.

import { type PublishState } from "../components/kit";
import { locale, t } from "../i18n";
import { screenHref } from "../state/screens";

/** The query parameter a publish bar hands the publish centre, naming the node it came from. */
export const RELEASE_NODE_PARAM = "node";

/**
 * The `addToRelease` prop for a bar publishing `node`, or `undefined` when there is no tenant yet.
 *
 * `undefined` rather than a dead link: without a tenant the publish centre has nothing to list, and
 * a button that lands on the context picker is a button that lied about where it was going.
 */
export function addToRelease(
  node: string,
  tenant: string,
): { href: string; label: string } | undefined {
  if (!tenant) {
    return undefined;
  }
  return {
    href: `${screenHref("releases", tenant, "")}?${RELEASE_NODE_PARAM}=${encodeURIComponent(node)}`,
    label: t("publish.addToRelease"),
  };
}

/** The line under a publish control, for a node published at `publishedAtMs`. */
export function describePublish(state: PublishState, publishedAtMs: number | null): string {
  if (state === "never" || publishedAtMs === null) {
    return t("publish.never");
  }
  const when = new Intl.DateTimeFormat(locale(), {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(publishedAtMs));
  return state === "stale" ? t("publish.stale", { when }) : t("publish.published", { when });
}
