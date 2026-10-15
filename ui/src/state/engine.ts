// The one engine every piece of store data loads through (ADR-0173 decision 2).
//
// Each piece is declared once, in `./registry`, as an `Entry`: what kind of data it is, where it is
// read, how its body is applied to the store, whether the till may be drawn without it, and which
// configuration nodes it derives from. This module runs them all the same way:
//
// - **States.** An entry is loading, loaded, *absent* (an edge too old to serve the route answered
//   `404`, which is an answer, not a failure) or failing. `loaded` means it has applied a body at
//   least once; a later failure keeps what it applied.
// - **Retry with backoff.** A failure the edge may recover from (no answer, a `5xx`, `408`, `429`)
//   is retried after 0.5 s, doubling to 15 s, with ±20% jitter. A refusal is not: a `401` sends the
//   device to pair and a `403` to sign in, as every other call does.
// - **Revalidation.** Each read names the tag of the copy it holds, and an unchanged copy costs a
//   bodiless `304`.
// - **When it reads again.** On a `resync`, every entry: the edge's answer to a live link that
//   cannot be resumed, which is every link that holds no position, its first connect included; and
//   on `config_applied`, only the entries that derive from the nodes the frame names, or every one
//   where it names none.
// - **The boot gate.** Once the store's load starts, the till is drawn when every required entry
//   is loaded or absent. *Loaded* means the edge confirmed it in this session, with a body or a
//   `304`: a copy from the snapshot never opens the gate.
// - **The snapshot** (`./snapshot`). A configuration entry the edge sends is kept, with its tag.
//   At the next start its tag is sent with the first read, and its body is applied only once the
//   edge answers `304` to that tag, so nothing is sold against a price the edge has not confirmed.
//   An entry that holds no money (`drawStale`) is drawn from the snapshot at once, as stale.
//
// Nothing here knows a currency, a locale or a country: those are entries like any other.

import { createSignal } from "solid-js";
import { createStore } from "solid-js/store";

import { ApiError, revalidate } from "../api/client";
import { type Restored, restoreSnapshot, saveSnapshot } from "./snapshot";

// Configuration is what the store sells and how; live data is what it is doing and who is doing it
// (the session and its permissions, the drawers, the shift, the open orders). Only configuration
// may ever be kept on the device (ADR-0173 decision 3), so the kind is declared on every entry.
export type EntryKind = "configuration" | "live";

export interface Entry {
  // The entry's name, and the `data.name.<key>` translation key the screens show it by.
  readonly key: string;
  readonly kind: EntryKind;
  // Where it is read.
  readonly path: () => string;
  // A read of more than one route, in place of `path`. It names no tag, so it is always answered
  // in full.
  readonly read?: () => Promise<unknown>;
  // Applies a body the edge sent to the store.
  readonly apply: (body: unknown) => void;
  // Whether the till waits for it before it is drawn.
  readonly required: boolean;
  // The configuration nodes it derives from.
  readonly nodes: readonly string[];
  // Whether it is also read again for a changed node no entry names.
  readonly unclaimedNodes?: boolean;
  // Read only while this holds.
  readonly when?: () => boolean;
  // A configuration entry is kept in the snapshot unless this is `false`; a live one never is.
  readonly persist?: boolean;
  // Drawn from the snapshot before the edge confirms it. Only for an entry that holds no money.
  readonly drawStale?: boolean;
}

interface Status {
  loading: boolean;
  loaded: boolean;
  // Drawn from the snapshot, and not yet confirmed by the edge.
  stale: boolean;
  absent: boolean;
  failing: boolean;
  failures: number;
}

const FIRST_DELAY_MS = 500;
const CEILING_MS = 15_000;

const [status, setStatus] = createStore<Record<string, Status>>({});
const [started, setStarted] = createSignal(false);
let entries: readonly Entry[] = [];
// One tag, one timer and one read in flight per entry, so each is bounded by the registry.
const etags = new Map<string, string>();
const timers = new Map<string, ReturnType<typeof setTimeout>>();
const inFlight = new Map<string, Promise<void>>();
// The entries whose fresh read waits behind the one out.
const queued = new Set<string>();
// What the snapshot held at this start, until the edge confirms or replaces each entry.
const restored = new Map<string, Restored>();
let restoring: Promise<void> | null = null;

function persisted(found: Entry): boolean {
  return found.kind === "configuration" && found.persist !== false;
}

// Reads the snapshot once, before the first reads, so they can name its tags.
function restore(): Promise<void> {
  restoring ??= restoreSnapshot().then((held) => {
    for (const found of entries) {
      const copy = held.get(found.key);
      if (copy === undefined || !persisted(found)) {
        continue;
      }
      restored.set(found.key, copy);
      if (found.drawStale === true) {
        found.apply(copy.body);
        setStatus(found.key, "stale", true);
      }
    }
  });
  return restoring;
}

// Declares the entries. Called once, by `./registry`.
export function register(list: readonly Entry[]): void {
  entries = list;
  for (const entry of list) {
    setStatus(entry.key, {
      loading: false,
      loaded: false,
      stale: false,
      absent: false,
      failing: false,
      failures: 0,
    });
  }
}

function entry(key: string): Entry | undefined {
  return entries.find((candidate) => candidate.key === key);
}

// Whether the edge may answer differently if asked again.
export function retryable(error: unknown): boolean {
  if (!(error instanceof ApiError)) {
    return true;
  }
  return error.status >= 500 || error.status === 408 || error.status === 429;
}

export function backoff(failures: number): number {
  const delay = Math.min(FIRST_DELAY_MS * 2 ** (failures - 1), CEILING_MS);
  return delay * (0.8 + Math.random() * 0.4);
}

async function read(found: Entry): Promise<void> {
  clearTimeout(timers.get(found.key));
  timers.delete(found.key);
  setStatus(found.key, "loading", true);
  try {
    const copy = restored.get(found.key);
    const tag = etags.get(found.key) ?? copy?.etag ?? null;
    const answer =
      found.read === undefined
        ? await revalidate<unknown>(found.path(), tag)
        : { notModified: false as const, body: await found.read(), etag: null };
    restored.delete(found.key);
    if (answer.notModified) {
      // The edge confirmed the snapshot's copy: apply it, unless it is already drawn.
      if (copy !== undefined && etags.get(found.key) === undefined) {
        etags.set(found.key, copy.etag);
        if (status[found.key]?.stale !== true) {
          found.apply(copy.body);
        }
      }
    } else {
      found.apply(answer.body);
      if (answer.etag === null) {
        etags.delete(found.key);
      } else {
        etags.set(found.key, answer.etag);
        if (persisted(found)) {
          saveSnapshot(found.key, answer.etag, answer.body);
        }
      }
    }
    setStatus(found.key, {
      loading: false,
      loaded: true,
      stale: false,
      absent: false,
      failing: false,
      failures: 0,
    });
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) {
      setStatus(found.key, { loading: false, absent: true, failing: false, failures: 0 });
      return;
    }
    const failures = (status[found.key]?.failures ?? 0) + 1;
    setStatus(found.key, { loading: false, failing: true, failures });
    if (retryable(error)) {
      timers.set(
        found.key,
        setTimeout(() => void load(found.key), backoff(failures)),
      );
    }
  }
}

// Reads one entry now, unless a read of it is already on its way, or it is not wanted.
//
// `fresh` asks for a read that starts now: one already on its way may have been answered before
// what the caller needs it to include, so a fresh read follows it rather than joining it. At most
// one read waits behind the one out, so the reads of an entry stay bounded.
export function load(key: string, fresh = false): Promise<void> {
  const found = entry(key);
  if (found === undefined || (found.when !== undefined && !found.when())) {
    return Promise.resolve();
  }
  const pending = inFlight.get(key);
  if (pending !== undefined && (!fresh || queued.has(key))) {
    return pending;
  }
  let start = read;
  if (pending !== undefined) {
    queued.add(key);
    start = async (each) => {
      await pending;
      queued.delete(key);
      await read(each);
    };
  }
  const next = start(found).finally(() => {
    if (inFlight.get(key) === next) {
      inFlight.delete(key);
    }
  });
  inFlight.set(key, next);
  return next;
}

// Reads every entry but those in `except`: the store's load at the boot gate and at sign-in, and a
// `resync`, where each read that names a tag costs a `304` when nothing changed.
export async function loadAll(except: readonly string[] = [], fresh = false): Promise<void> {
  setStarted(true);
  await restore();
  await Promise.all(
    entries.filter((each) => !except.includes(each.key)).map((each) => load(each.key, fresh)),
  );
}

// Applies a body for `key` that was read outside the engine, as the boot gate's own read of the
// session is, and counts the entry loaded, so the store's load need not read it again.
export function prime(key: string, body: unknown): void {
  const found = entry(key);
  if (found === undefined) {
    return;
  }
  found.apply(body);
  setStatus(key, { loading: false, loaded: true, absent: false, failing: false, failures: 0 });
}


// The live link is back after a drop, or fell behind and cannot be replayed: everything is read
// again, once the store's load has started, and an unchanged copy costs a `304`.
export async function readAgain(): Promise<void> {
  if (started()) {
    // Fresh, because a read already out may have been answered before what this one must include.
    await loadAll([], true);
  }
}

// Reads what a new configuration may have changed. `changed` is the frame's `changed_nodes`: absent
// from an edge that cannot say, which reads every entry that derives from configuration.
export async function configApplied(changed: readonly string[] | undefined): Promise<void> {
  if (changed === undefined) {
    const derived = entries.filter((each) => each.nodes.length > 0 || each.unclaimedNodes === true);
    await Promise.all(derived.map((each) => load(each.key)));
    return;
  }
  const claimed = new Set(entries.flatMap((each) => each.nodes));
  const unclaimed = changed.some((node) => !claimed.has(node));
  const affected = entries.filter(
    (each) =>
      each.nodes.some((node) => changed.includes(node)) ||
      (unclaimed && each.unclaimedNodes === true),
  );
  await Promise.all(affected.map((each) => load(each.key)));
}

// Reads again what derives from `node`, as when the edge has said that node changed: an item brought
// back reads the price books again.
export function reload(node: string): Promise<void> {
  return configApplied([node]);
}

// Reads every entry that is failing now, from the start of its backoff: the Retry button.
export async function retryFailing(): Promise<void> {
  const failing = entries.filter((each) => status[each.key]?.failing === true);
  for (const each of failing) {
    setStatus(each.key, "failures", 0);
  }
  await Promise.all(failing.map((each) => load(each.key)));
}

// Whether the till may be drawn: before the store's load starts there is nothing to wait for, and
// after it every required entry has loaded or been answered absent.
export function storeReady(): boolean {
  return (
    !started() ||
    entries.every((each) => !each.required || status[each.key]?.loaded || status[each.key]?.absent)
  );
}

// The required entries the till is still waiting for.
export function waitingFor(): readonly string[] {
  return entries
    .filter((each) => each.required && !status[each.key]?.loaded && !status[each.key]?.absent)
    .map((each) => each.key);
}

// The entries that are failing and hold nothing: what the banner names. One that loaded before and
// failed since keeps what it has, and is retried without a word, as is one drawn from the snapshot.
export function notLoaded(): readonly string[] {
  return entries
    .filter((each) => status[each.key]?.failing && !status[each.key]?.loaded && !status[each.key]?.stale)
    .map((each) => each.key);
}

// Why an act that needs `key` cannot be taken now, as a translation key, or `null` where it can.
export function unavailable(key: string): "data.unavailable_failed" | "data.unavailable_loading" | null {
  const held = status[key];
  if (held === undefined || held.loaded || held.stale || held.absent || !started()) {
    return null;
  }
  return held.failing ? "data.unavailable_failed" : "data.unavailable_loading";
}
