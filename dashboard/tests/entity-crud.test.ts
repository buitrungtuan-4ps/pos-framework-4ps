// The authoring lifecycle ([ADR-0121](../../docs/adr/0121-one-way-to-author-an-entity.md) §3–4).
//
// Two of these pin properties the hand-rolled versions across the console got wrong rather than
// merely wrote out longhand, and they are the reason this abstraction exists at all:
//
//   * a refused write keeps its form open, carrying the refusal's own message, because the
//     operator's typing is the only copy of their intent (ADR-0094 makes a stale refusal routine);
//   * `saving` belongs to one lifecycle, so a screen authoring two entity types does not freeze
//     one while the other saves — the audit's "one shared busy flag disables every button".

import { describe, expect, it } from "vitest";

import { useEntityCrud } from "../src/lib/entity-crud";
import { ApiError } from "../src/api/client";

type Store = { readonly store_id: string; readonly name: string };

const LTT: Store = { store_id: "01M221BB8BB5SESQDB895SJHJS", name: "4P's Le Thanh Ton" };
const BT: Store = { store_id: "01M221KB3CN3XVWHY1NSCKA48H", name: "4P's Ben Thanh" };

/** A promise that only settles when the test says so — for observing `saving` mid-flight. */
function deferred<T>() {
  let settle!: (value: T) => void;
  let fail!: (reason: unknown) => void;
  const promise = new Promise<T>((resolve, reject) => {
    settle = resolve;
    fail = reject;
  });
  return { promise, settle, fail };
}

describe("the authoring lifecycle", () => {
  it("starts closed, with nothing selected and nothing to say", () => {
    const crud = useEntityCrud<Store>();
    expect(crud.mode()).toBe("idle");
    expect(crud.subject()).toBeNull();
    expect(crud.saving()).toBe(false);
    expect(crud.error()).toBe("");
  });

  it("opens the create form with no subject", () => {
    const crud = useEntityCrud<Store>();
    crud.create();
    expect(crud.mode()).toBe("creating");
    expect(crud.subject()).toBeNull();
  });

  it("opens the edit form on the row it was given", () => {
    const crud = useEntityCrud<Store>();
    crud.edit(LTT);
    expect(crud.mode()).toBe("editing");
    expect(crud.subject()).toEqual(LTT);
  });

  // `confirming` is its own mode so a destructive question and a form are never the same state —
  // `FormPanel` opens for the first two and `ConfirmDialog` for this one.
  it("keeps confirming distinct from editing", () => {
    const crud = useEntityCrud<Store>();
    crud.confirm(BT);
    expect(crud.mode()).toBe("confirming");
    expect(crud.subject()).toEqual(BT);
  });

  it("forgets the subject and the message when closed", async () => {
    const crud = useEntityCrud<Store>();
    crud.edit(LTT);
    await crud.run(() => Promise.reject(new Error("nope")));
    expect(crud.error()).not.toBe("");

    crud.close();

    expect(crud.mode()).toBe("idle");
    expect(crud.subject()).toBeNull();
    expect(crud.error()).toBe("");
  });

  it("closes on a write that succeeds, and says it succeeded", async () => {
    const crud = useEntityCrud<Store>();
    crud.create();

    const saved = await crud.run(() => Promise.resolve({ ok: true }));

    expect(saved).toBe(true);
    expect(crud.mode()).toBe("idle");
    expect(crud.saving()).toBe(false);
    expect(crud.error()).toBe("");
  });

  // The property, not the mechanism: a refusal must not cost the operator their typing.
  it("keeps the form open on a refused write, carrying its message", async () => {
    const crud = useEntityCrud<Store>();
    crud.edit(LTT);

    const saved = await crud.run(() =>
      // `ApiError` is (status, message, canonical) — the message is what an operator reads.
      Promise.reject(new ApiError(412, "Somebody else saved this first.", "FAILED_PRECONDITION")),
    );

    expect(saved).toBe(false);
    expect(crud.mode()).toBe("editing");
    expect(crud.subject()).toEqual(LTT);
    expect(crud.error()).toBe("Somebody else saved this first.");
    expect(crud.saving()).toBe(false);
  });

  it("clears the previous message when the next write starts", async () => {
    const crud = useEntityCrud<Store>();
    crud.create();
    await crud.run(() => Promise.reject(new Error("first")));
    expect(crud.error()).toContain("first");

    const inFlight = deferred<void>();
    const second = crud.run(() => inFlight.promise);
    expect(crud.error()).toBe("");

    inFlight.settle();
    await second;
  });

  it("is busy only while the write is in flight", async () => {
    const crud = useEntityCrud<Store>();
    crud.create();
    const inFlight = deferred<void>();

    const running = crud.run(() => inFlight.promise);
    expect(crud.saving()).toBe(true);

    inFlight.settle();
    await running;
    expect(crud.saving()).toBe(false);
  });

  it("stops being busy even when the write throws", async () => {
    const crud = useEntityCrud<Store>();
    crud.create();
    await crud.run(() => Promise.reject(new Error("boom")));
    expect(crud.saving()).toBe(false);
  });

  // The audit's finding, as a property: two lifecycles on one screen are independent, so saving a
  // brand cannot disable the store table's buttons.
  it("does not make a sibling lifecycle busy", async () => {
    const stores = useEntityCrud<Store>();
    const brands = useEntityCrud<Store>();
    const inFlight = deferred<void>();

    const running = brands.run(() => inFlight.promise);

    expect(brands.saving()).toBe(true);
    expect(stores.saving()).toBe(false);

    inFlight.settle();
    await running;
  });

  it("shows a client-side refusal without opening or closing anything", () => {
    const crud = useEntityCrud<Store>();
    crud.create();
    crud.refuse("A name is required.");
    expect(crud.mode()).toBe("creating");
    expect(crud.error()).toBe("A name is required.");
  });
});
