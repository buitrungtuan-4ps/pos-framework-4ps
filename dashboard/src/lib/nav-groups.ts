// Which nav group is open, and how the console remembers.
//
// # Why one at a time
//
// Six groups and thirty entries were expanded all of the time; collapsing them by default (#261)
// fixed the length but left every group free to be open at once, so the nav could still be a
// column an operator scrolls. Worse, the open group and the closed one were drawn identically
// apart from a triangle, and the *entry* an operator was standing on was drawn in the same
// `surface-raised` as any entry under the pointer — so the two questions "which section am I in"
// and "which screen am I on" both had no visible answer.
//
// So: one group is open. That is what keeps the nav a fixed, short shape — eight headings plus at
// most six entries — no matter how many groups the console grows. The cost is honest and worth
// naming: reaching a screen in another group is now a click on that heading and then the entry,
// where before it could be one click if that group happened to be open. The command palette is the
// one-click path for an operator who knows where they are going, and it does not care about groups
// at all.
//
// # The rule, and why "remembered" is a separate question from "open"
//
// With nothing remembered, the open group is the one holding the screen you are looking at. That
// keeps the nav short without ever hiding where you are, and it needs no special case for the
// landing screen — the store hub is in Overview, so Overview is open when you arrive.
//
// An answer the operator gave wins, and it is one answer, not a set: the group they opened, or
// `null` for "I closed it and want the headings alone". `undefined` is the third state, "they have
// never said", and it is the one the containment rule answers. A full open/closed snapshot could
// not tell "the operator closed this" from "this happened to be closed when the snapshot was
// taken", so the next release's default could never change without silently overriding somebody's
// choice — or being silently overridden by one they never made.

import type { MessageKey } from "../i18n";
import { NAV_GROUPS, type ScreenId } from "../state/screens";

/** The operator's answer: the group they opened, or `null` for "none of them". */
export type Opened = MessageKey | null;

const STORAGE_KEY = "pos.dashboard.navGroups";

/** Whether `items` holds the screen the operator is on. */
export function holdsCurrent(
  items: readonly ScreenId[],
  current: ScreenId | undefined,
): boolean {
  return current !== undefined && items.includes(current);
}

/**
 * Whether a group renders open.
 *
 * `opened` being `undefined` is the whole point of the signature: it is the third state, "the
 * operator has never said", and it is what the containment rule answers.
 */
export function groupOpen(args: {
  readonly key: MessageKey;
  readonly items: readonly ScreenId[];
  readonly opened: Opened | undefined;
  readonly current: ScreenId | undefined;
}): boolean {
  if (args.opened !== undefined) {
    return args.opened === args.key;
  }
  return holdsCurrent(args.items, args.current);
}

/**
 * The answer this browser holds, or `undefined` for "nothing remembered".
 *
 * Anything unreadable, unparseable, the wrong shape, or naming a group that no longer exists reads
 * as "nothing remembered", which falls back to the containment rule: a corrupt key must degrade to
 * a working nav, never to a nav with no group open. That last case is not hypothetical — the key
 * used to hold a map of every group the operator had ever toggled, and a rename of a group would
 * otherwise leave the nav pointing at a heading that is gone. Private windows and blocked site
 * data throw on access, so the read is guarded.
 */
export function loadOpened(): Opened | undefined {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === null) {
      return undefined;
    }
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
      return undefined;
    }
    const opened: unknown = (parsed as { readonly opened?: unknown }).opened;
    if (opened === null) {
      return null;
    }
    if (typeof opened === "string" && NAV_GROUPS.some((group) => group.key === opened)) {
      return opened as MessageKey;
    }
    return undefined;
  } catch {
    return undefined;
  }
}

/** Records the answer, returning it. Persistence is a convenience; a failure is not an error. */
export function rememberOpened(opened: Opened): Opened {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ opened }));
  } catch {
    // The nav still works for this session; only the memory is lost.
  }
  return opened;
}
