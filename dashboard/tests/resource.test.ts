// The read every screen shares, and the four ways a hand-rolled one goes wrong (Wave 4 · PR-3, D5).
//
// Thirty screens each wrote their own fetch-into-two-signals, and the Refresh button was the
// apology for what none of them did afterwards. What is pinned here is not "it fetches" — that is
// one line and no test would be written for it — but the four behaviours the hand-rolled versions
// did not have, each of which is a way a list quietly stops matching the server:
//
//   * a refusal is a *third* state, not an empty list. A screen that renders "we could not look"
//     as "nothing here" tells an operator their shop has no staff when their role simply cannot
//     read the roster;
//   * `refetch` re-reads with the context the last read used, so a mutation does not have to hand
//     the tenant and store back — and, before any read has happened, does nothing rather than
//     firing with an empty tenant id (the refusal the F0 context gate exists to prevent);
//   * a slow read that lands after a fast one is discarded. This is what makes switching stores
//     twice in a second show the store you are on rather than the one you left;
//   * focus revalidation is bounded by staleness, so alt-tabbing twice costs one request.

import { createRoot } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createAdminResource } from "../src/lib/resource";
import { selectStore, selectTenant } from "../src/state/session";

const TENANT = "01M22190WCY5PS7KCA7ET7H679";
const STORE = "01STOREAAAAAAAAAAAAAAAAAAA";
const OTHER_STORE = "01STOREBBBBBBBBBBBBBBBBBBB";

/** Lets the microtask queue drain so a resolved read has reached the signal. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

beforeEach(() => {
  localStorage.clear();
  selectTenant(TENANT, "Pizza 4P's Vietnam");
  selectStore(STORE, "Bến Thành");
});

afterEach(() => {
  vi.useRealTimers();
});

describe("an admin resource", () => {
  it("keeps a refusal apart from an empty answer", async () => {
    await createRoot(async (dispose) => {
      const resource = createAdminResource(
        () => Promise.reject(new Error("the caller is not permitted")),
        { scope: "store" },
      );
      await settle();
      expect(resource.state().state).toBe("failed");
      // The distinction the hand-rolled screens lost: no value, and a reason.
      expect(resource.value()).toBeNull();
      expect(resource.state()).toMatchObject({ message: expect.stringContaining("permitted") });
      dispose();
    });
  });

  it("re-reads with the context the last read used, so a mutation need not pass it back", async () => {
    await createRoot(async (dispose) => {
      const read = vi.fn().mockResolvedValue(["a"]);
      const resource = createAdminResource(read, { scope: "store" });
      await settle();
      expect(read).toHaveBeenCalledWith(TENANT, STORE);

      read.mockResolvedValue(["a", "b"]);
      await resource.refetch();
      expect(read).toHaveBeenCalledTimes(2);
      expect(read).toHaveBeenLastCalledWith(TENANT, STORE);
      expect(resource.value()).toEqual(["a", "b"]);
      dispose();
    });
  });

  it("does nothing on a refetch before the first read, rather than firing without a context", async () => {
    await createRoot(async (dispose) => {
      selectTenant("", "");
      selectStore("", "");
      const read = vi.fn().mockResolvedValue([]);
      const resource = createAdminResource(read, { scope: "store" });
      await resource.refetch();
      await settle();
      // The gate held: no read at all, rather than one with an empty tenant id that the server
      // would refuse with `tenant_id … is not a ULID`.
      expect(read).not.toHaveBeenCalled();
      dispose();
    });
  });

  it("discards a slow read that lands after a newer one", async () => {
    await createRoot(async (dispose) => {
      const resolvers: ((rows: string[]) => void)[] = [];
      const read = vi.fn(
        () => new Promise<string[]>((resolve) => resolvers.push(resolve)),
      );
      const resource = createAdminResource(read, { scope: "store" });
      await settle();

      // A second read starts — the operator switched store — and answers first.
      void resource.refetch();
      await settle();
      expect(resolvers).toHaveLength(2);
      resolvers[1]?.(["the store I am on"]);
      await settle();
      expect(resource.value()).toEqual(["the store I am on"]);

      // The first read finally lands. It must not overwrite what is on screen.
      resolvers[0]?.(["the store I left"]);
      await settle();
      expect(resource.value()).toEqual(["the store I am on"]);
      dispose();
    });
  });

  it("re-reads when the store changes, so a screen cannot show the previous shop's rows", async () => {
    await createRoot(async (dispose) => {
      const read = vi.fn().mockResolvedValue([]);
      createAdminResource(read, { scope: "store" });
      await settle();
      expect(read).toHaveBeenCalledTimes(1);

      selectStore(OTHER_STORE, "Xuân Thủy");
      await settle();
      expect(read).toHaveBeenCalledTimes(2);
      expect(read).toHaveBeenLastCalledWith(TENANT, OTHER_STORE);
      dispose();
    });
  });

  it("revalidates on focus only once the read is stale", async () => {
    await createRoot(async (dispose) => {
      const read = vi.fn().mockResolvedValue([]);
      createAdminResource(read, { scope: "store", revalidateOnFocus: true, staleAfterMs: 50 });
      await settle();
      expect(read).toHaveBeenCalledTimes(1);

      // Straight back to the tab: the rows are seconds old and a second request buys nothing.
      window.dispatchEvent(new Event("focus"));
      await settle();
      expect(read).toHaveBeenCalledTimes(1);

      await new Promise((resolve) => setTimeout(resolve, 60));
      window.dispatchEvent(new Event("focus"));
      await settle();
      expect(read).toHaveBeenCalledTimes(2);
      dispose();
    });
  });
});
