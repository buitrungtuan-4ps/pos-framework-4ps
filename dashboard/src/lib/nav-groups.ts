// Which nav groups are open, and how the console remembers.
//
// # Why the nav needed collapsing
//
// Six groups, thirty entries, all of them expanded all of the time. On a wide screen that is a
// column an operator scrolls past to reach the two screens they use; under `md` it was thirty
// wrapped chips stacked above every page's content, so the actual page began below the fold on a
// tablet held in portrait — the shape most likely in a shop.
//
// # The rule, and why "remembered" is a separate question from "open"
//
// A group with no remembered answer is open when it holds the screen you are looking at. That keeps
// the nav short without ever hiding where you are, and it needs no special case for the landing
// screen — the store hub is in Overview, so Overview is open when you arrive.
//
// An answer the operator gave wins, in both directions and permanently. That distinction is why the
// remembered state is a sparse map of explicit toggles rather than a full open/closed snapshot: a
// snapshot cannot tell "the operator closed Master data" from "Master data happened to be closed
// when the snapshot was taken", so the next release's default could never change without silently
// overriding somebody's choice — or being silently overridden by one they never made.

import type { MessageKey } from "../i18n";
import type { ScreenId } from "../state/screens";

/** The operator's explicit answers, keyed by the group's i18n key — its stable identity. */
export type Remembered = Partial<Record<MessageKey, boolean>>;

const STORAGE_KEY = "pos.dashboard.navGroups";

/**
 * Whether a group renders open.
 *
 * `remembered` being `undefined` is the whole point of the signature: it is the third state, "the
 * operator has never said", and it is what the containment rule answers.
 */
export function groupOpen(args: {
  readonly items: readonly ScreenId[];
  readonly remembered: boolean | undefined;
  readonly current: ScreenId | undefined;
}): boolean {
  if (args.remembered !== undefined) {
    return args.remembered;
  }
  return args.current !== undefined && args.items.includes(args.current);
}

/**
 * The remembered answers this browser holds.
 *
 * Anything unreadable, unparseable or the wrong shape reads as "nothing remembered", which falls
 * back to the containment rule: a corrupt key must degrade to a working nav, never to a nav with no
 * groups open. Private windows and blocked site data throw on access, so the read is guarded.
 */
export function loadRemembered(): Remembered {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === null) {
      return {};
    }
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
      return {};
    }
    const out: Remembered = {};
    for (const [key, value] of Object.entries(parsed)) {
      if (typeof value === "boolean") {
        out[key as MessageKey] = value;
      }
    }
    return out;
  } catch {
    return {};
  }
}

/** Records one answer, returning the new map. Persistence is a convenience; a failure is not an error. */
export function remember(current: Remembered, group: MessageKey, open: boolean): Remembered {
  const next: Remembered = { ...current, [group]: open };
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    // The nav still works for this session; only the memory is lost.
  }
  return next;
}
