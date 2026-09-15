// When each configuration node was last published, for one store (finding **F6**).
//
// # Why a module rather than a read in each screen
//
// Thirteen screens ask the same question — "when did *my* node last reach this store?" — and the
// answer is one request. Written per screen it would be thirteen copies of the same fetch, the same
// 404-means-nothing-published handling, and the same "reload it after I publish" that every one of
// them would get subtly differently. Here it is once, and a screen's whole use of it is two lines.
//
// The comparison it feeds is deliberately split between the two sides that already hold the numbers:
// the cloud says when the node was published, the screen says when its own data last changed
// (`PublishBar`'s `editedAtMs`), and `publishState` in the kit puts them together. Neither side has
// to learn the other's business, and no new backend read maps thirteen nodes to thirteen tables.

import { createSignal } from "solid-js";

import { api } from "../api/client";
import type { ConfigNode, ConfigNodes } from "../api/types";
import { onScopedContext } from "./scoped";
import { storeId, tenantId } from "../state/session";

const EMPTY: ConfigNodes = { current_version_id: null, nodes: [] };

/**
 * The newest `updated_at_ms` across a collection, or `null` when none of it carries one.
 *
 * What a publish bar compares its publish date against. A collection is what a config node is
 * compiled from — every reason code, every station — so "when was this last edited" is the newest
 * edit in the set, not any one row's.
 *
 * `null` rather than `0` for an empty or date-less collection, because those two mean different
 * things and the bar treats them differently: `null` is "cannot tell", which suppresses the
 * staleness claim entirely, while `0` would be an instant in 1970 and therefore always older than
 * the publish — a confident "published" that happens to be right for the wrong reason.
 */
export function lastEditedMs(
  rows: readonly { readonly updated_at_ms?: number }[] | null | undefined,
): number | null {
  if (!rows) {
    return null;
  }
  let newest: number | null = null;
  for (const row of rows) {
    const at = row.updated_at_ms;
    if (at !== undefined && (newest === null || at > newest)) {
      newest = at;
    }
  }
  return newest;
}

/**
 * Which nodes the cloud has published that the store is not yet running (finding **F4**).
 *
 * A store holds **one** config version, so "is this store up to date" has always been answerable —
 * and useless, because the answer was two ULIDs side by side and an operator has no way to tell
 * which of thirteen nodes the difference is in. Comparing each node's own version against the held
 * one names them.
 *
 * The comparison is a string comparison, and that is not a shortcut: a `ConfigVersionId` is a ULID,
 * which is lexicographically ordered by the timestamp it starts with. A node published after the
 * version the store holds sorts above it.
 *
 * A node with no recorded version is **not** reported stale. It is published — it is in the
 * effective document — and this read cannot tell when, so calling it stale would put a red line on
 * a store that is fine.
 */
export function staleNodes(
  nodes: readonly ConfigNode[],
  heldVersionId: string | null,
): readonly string[] {
  if (heldVersionId === null) {
    // The store holds nothing at all. That is "not installed yet", which the hub's own posture
    // rules already say better than a list of thirteen node names would.
    return [];
  }
  return nodes
    .filter((row) => row.version_id !== null && row.version_id > heldVersionId)
    .map((row) => row.node);
}

/** What a screen gets: the per-node dates, and a way to re-read them after it publishes. */
export type PublishedNodes = {
  /** When `node` was last published to this store, or `null` — never, or not recorded. */
  readonly publishedAtMs: (node: string) => number | null;
  /** Every node, for a caller that wants the whole picture rather than one node's date. */
  readonly nodes: () => readonly ConfigNode[];
  /** Re-read. A screen calls this after its own publish so the bar stops saying "stale". */
  readonly refresh: () => Promise<void>;
};

/**
 * Reads the store's per-node publish dates, and again whenever the store changes.
 *
 * Failure is silent on purpose: this is a *label*, not a gate. A console that refused to draw the
 * tax editor because it could not read a date would be worse than one whose bar says "never
 * published" until the next read succeeds — and the publish itself still reports its own outcome.
 */
export function usePublishedNodes(): PublishedNodes {
  const [read, setRead] = createSignal<ConfigNodes>(EMPTY);

  const load = async () => {
    try {
      setRead(await api.configNodes(tenantId(), storeId()));
    } catch {
      setRead(EMPTY);
    }
  };

  onScopedContext("store", () => void load());

  return {
    publishedAtMs: (node: string) =>
      read().nodes.find((row) => row.node === node)?.at_ms ?? null,
    nodes: () => read().nodes,
    refresh: load,
  };
}
