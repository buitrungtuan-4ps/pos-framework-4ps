// The three things a `PublishBar` can say, in the console's words.
//
// The kit decides *which* of the three a node is in (`publishState`); this turns that into a
// sentence. One function rather than three copies per screen, because thirteen screens saying the
// same thing three slightly different ways is the finding this wave is closing (F13), and a screen
// that wants its own wording can still pass its own `describe`.

import { type PublishState } from "../components/kit";
import { locale, t } from "../i18n";

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
