// The till's snapshot of the store's configuration (ADR-0173 decision 3).
//
// **Configuration only.** The engine writes an entry here only when its kind is `configuration`
// and it does not opt out: the price books, the button layout, the money settings and the reason
// codes. Never the session, permissions, PINs, tokens, staff or customer data, drawers, shifts or
// orders, and not the floor, whose read carries live table states. None of it is personal data.
//
// **Whose it is.** Each record names the store server it came from and this device's pairing
// (`api/credentials.ts`). A record from another store server, from before this device paired
// again, of another schema, or older than seven days is never used, and is deleted when read.
//
// **When it goes.** On a person signing out, and when the device is unpaired, by a reset or by a
// `401` that clears its token. Not on the idle lock, which keeps the device in the same service.
//
// **Where there is none.** A private window, blocked storage or a browser without IndexedDB has no
// snapshot. Every call here then answers "nothing" and says nothing: the till reads the edge, as it
// always did.

import { edgeBase, onPairingForgotten, pairingId } from "../api/credentials";

const DATABASE = "pos-till-snapshot";
const STORE = "entries";
// Raised whenever a record's shape changes; a record of any other version is discarded.
const SCHEMA = 1;
const RETENTION_MS = 7 * 24 * 60 * 60 * 1000;
// How long the till waits for its snapshot before it reads the edge without one.
const RESTORE_WAIT_MS = 300;

interface SnapshotRecord {
  readonly key: string;
  readonly schema: number;
  readonly identity: string;
  readonly savedTime: number;
  readonly etag: string;
  readonly body: unknown;
}

// What a restored entry holds: the edge's tag, and the body that tag names.
export interface Restored {
  readonly etag: string;
  readonly body: unknown;
}

// The store server and the pairing a record belongs to, or `null` where this device is not paired.
function identity(): string | null {
  const pairing = pairingId();
  return pairing === null ? null : `${edgeBase()}|${pairing}`;
}

let opened: Promise<IDBDatabase | null> | null = null;

function database(): Promise<IDBDatabase | null> {
  opened ??= new Promise((resolve) => {
    try {
      const request = indexedDB.open(DATABASE, 1);
      request.onupgradeneeded = () => request.result.createObjectStore(STORE, { keyPath: "key" });
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => resolve(null);
      request.onblocked = () => resolve(null);
    } catch {
      resolve(null);
    }
  });
  return opened;
}

// Runs `act` on the store in one transaction; `null` wherever IndexedDB is not there to run it.
async function withStore<T>(
  mode: IDBTransactionMode,
  act: (store: IDBObjectStore) => IDBRequest<T>,
): Promise<T | null> {
  const db = await database();
  if (db === null) {
    return null;
  }
  return new Promise((resolve) => {
    try {
      const request = act(db.transaction(STORE, mode).objectStore(STORE));
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => resolve(null);
    } catch {
      resolve(null);
    }
  });
}

function usable(record: SnapshotRecord, owner: string, now: number): boolean {
  return record.schema === SCHEMA && record.identity === owner && now - record.savedTime <= RETENTION_MS;
}

// Every record this device may use, by entry key; anything it may not is deleted. Gives up after
// `RESTORE_WAIT_MS`, so a slow or blocked store never holds the till back.
export async function restoreSnapshot(): Promise<ReadonlyMap<string, Restored>> {
  const owner = identity();
  if (owner === null) {
    return new Map();
  }
  const records = await Promise.race([
    withStore<SnapshotRecord[]>("readonly", (store) => store.getAll() as IDBRequest<SnapshotRecord[]>),
    new Promise<null>((resolve) => setTimeout(() => resolve(null), RESTORE_WAIT_MS)),
  ]);
  const restored = new Map<string, Restored>();
  const now = Date.now();
  for (const record of records ?? []) {
    if (usable(record, owner, now)) {
      restored.set(record.key, { etag: record.etag, body: record.body });
    } else {
      void withStore("readwrite", (store) => store.delete(record.key));
    }
  }
  return restored;
}

// Keeps what the edge just sent for `key` under the tag it sent, for the next start.
export function saveSnapshot(key: string, etag: string, body: unknown): void {
  const owner = identity();
  if (owner === null) {
    return;
  }
  const record: SnapshotRecord = { key, schema: SCHEMA, identity: owner, savedTime: Date.now(), etag, body };
  void withStore("readwrite", (store) => store.put(record));
}

// Deletes the whole snapshot: a person signing out, or the device being unpaired.
export function clearSnapshot(): Promise<unknown> {
  return withStore("readwrite", (store) => store.clear());
}

onPairingForgotten(() => void clearSnapshot());
