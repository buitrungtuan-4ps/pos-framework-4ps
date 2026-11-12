// A read answered after the person signed out writes nothing into the snapshot (ADR-0173 decision
// 3): its answer belongs to the session that ended. The replay "signing out clears the snapshot"
// found a record left behind when a read that set out before the sign-out landed after it.
//
// jsdom has no IndexedDB, so this stands in a store that keeps records in a map and answers every
// request on a later task, as the browser's does.

import { beforeEach, describe, expect, it, vi } from "vitest";

const records = new Map<string, unknown>();

function request<T>(result: () => T) {
  const answer: { result?: T; onsuccess?: () => void; onerror?: () => void } = {};
  setTimeout(() => {
    answer.result = result();
    answer.onsuccess?.();
  }, 0);
  return answer;
}

const objectStore = {
  put: (record: { key: string }) => request(() => records.set(record.key, record)),
  clear: () => request(() => records.clear()),
  delete: (key: string) => request(() => records.delete(key)),
  getAll: () => request(() => [...records.values()]),
};

const fakeIndexedDb = {
  open: () => {
    const opening: { result?: unknown; onsuccess?: () => void } = {};
    setTimeout(() => {
      opening.result = { transaction: () => ({ objectStore: () => objectStore }) };
      opening.onsuccess?.();
    }, 0);
    return opening;
  },
};

const settle = () => new Promise((resolve) => setTimeout(resolve, 10));

beforeEach(() => {
  records.clear();
  vi.stubGlobal("indexedDB", fakeIndexedDb);
});

describe("the till's snapshot across a sign-out", () => {
  it("keeps an answer to a read made in this session, and none to one made before the sign-out", async () => {
    const { rememberPairing } = await import("../src/api/credentials");
    const { clearSnapshot, saveSnapshot, snapshotGeneration } = await import("../src/state/snapshot");
    rememberPairing("http://127.0.0.1:8787", "a-device-token");

    saveSnapshot("menu", 'W/"1"', { items: [] }, snapshotGeneration());
    await settle();
    expect(records.size).toBe(1);

    // A read sets out, and the person signs out. The read is answered while the snapshot is being
    // cleared, as `signedOut` waits for the clear before the page leaves.
    const setOut = snapshotGeneration();
    const cleared = clearSnapshot();
    saveSnapshot("menu", 'W/"2"', { items: [] }, setOut);
    await cleared;
    await settle();
    expect(records.size, "the late answer is not kept").toBe(0);

    // The next person's reads are kept again.
    saveSnapshot("menu", 'W/"3"', { items: [] }, snapshotGeneration());
    await settle();
    expect(records.size).toBe(1);
  });
});
