// What a store hub card says, and in what hue — separated from the markup that draws it.
//
// # Why this is a module and not three ternaries in the JSX
//
// A store that was created in the console five minutes ago and has never been installed opened the
// hub in red: **Not reporting** and **Behind**, both `attention`, both technically true and both
// wrong as advice. Nothing is broken — the machine in the shop does not exist yet. An operator who
// is shown an alarm for the expected state of every new store learns to ignore the hue, which is
// the one thing the hue is for.
//
// The rules that separate "this shop has a fault" from "this shop has not started yet" are
// therefore pure functions over the fleet row, returning a message key and a tone. They are unit
// tested (`tests/store-posture.test.ts`) because the distinction is a judgement about meaning that
// a type cannot hold and a build cannot check: `config_current === false` compiles identically
// whether it means "the till is running an old menu" or "nobody has published a menu yet".
//
// A card's *support* line is unchanged by any of this and still carries the same fact in words — the
// hue stays a second channel, never the only one (ADR-0020's accessibility floor).

import type { FleetStore } from "../api/types";
import type { MessageKey } from "../i18n";

/**
 * How a card's headline reads at a glance. The support line always repeats the fact in words, so a
 * reader who cannot see the hue loses nothing. `plain` is for a figure that is neither good nor bad
 * on its own: money taken is not a fault when it is low.
 */
export type Tone = "ok" | "attention" | "idle" | "plain";

export function toneClass(tone: Tone): string {
  switch (tone) {
    case "ok":
      return "text-ok";
    case "attention":
      return "text-danger";
    case "idle":
      return "text-ink-muted";
    default:
      return "text-ink";
  }
}

/** A headline to print and the hue to print it in. */
export type Verdict = { readonly headline: MessageKey; readonly tone: Tone };

/** The fields these rules read. Narrower than [`FleetStore`] so a test can state a case in four lines. */
export type StoreFacts = Pick<
  FleetStore,
  "online" | "last_seen_at_ms" | "config_current" | "config_version_held" | "config_version_published"
>;

/**
 * Whether the store has never once checked in — provisioned in the console, not yet installed.
 *
 * `last_seen_at_ms` is the only field that can tell the difference, and it tells it exactly: the
 * cloud writes it on the first heartbeat and never clears it (ADR-0068). A store that has reported
 * even once and gone quiet is a different situation with the same `online: false`, and it is the
 * situation that deserves the alarm.
 */
export function neverInstalled(store: Pick<StoreFacts, "last_seen_at_ms">): boolean {
  return store.last_seen_at_ms === null;
}

/** The Online card: reporting, silent after having reported, or not installed yet. */
export function onlineVerdict(store: Pick<StoreFacts, "online" | "last_seen_at_ms">): Verdict {
  if (store.online) {
    return { headline: "hub.online.yes", tone: "ok" };
  }
  if (neverInstalled(store)) {
    return { headline: "hub.online.notInstalled", tone: "idle" };
  }
  return { headline: "hub.online.no", tone: "attention" };
}

/**
 * The Configuration card.
 *
 * The server derives `config_current` from held-vs-published and reports `false` when *either* side
 * is missing — "there is a gap to close either way" (`FleetStoreView::from_row`). True, and three
 * different gaps: nobody has published, the store has not collected, or the store is holding an
 * older version. Only the last is the store's fault, and only the last is an alarm.
 */
export function configVerdict(store: StoreFacts): Verdict {
  if (store.config_current) {
    return { headline: "hub.config.current", tone: "ok" };
  }
  if (store.config_version_published === null) {
    return { headline: "hub.config.notPublished", tone: "idle" };
  }
  if (neverInstalled(store)) {
    return { headline: "hub.config.notDelivered", tone: "idle" };
  }
  return { headline: "hub.config.behind", tone: "attention" };
}
