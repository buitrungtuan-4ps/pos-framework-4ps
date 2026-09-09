// The form shell ([ADR-0121](../../docs/adr/0121-one-way-to-author-an-entity.md) §1).
//
// `FormPanel` exists so that "how do I add a record" has one answer. What it owns is chrome —
// drawer or modal, the title, the footer, the disabled submit, the refusal banner, the close — and
// the point of testing it is that every screen inherits whatever it gets right or wrong. The three
// that would be invisible if broken:
//
//   * the panel opens for `creating`/`editing` and **not** for `confirming`, or a form appears over
//     a "are you sure you want to delete this" question;
//   * the submit is disabled while saving, or an impatient double-click sends the write twice;
//   * closing with unsaved input asks first, and a save in flight cannot be closed out from under.

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import { FormPanel } from "../src/components/kit";
import { TextField } from "../src/components/ui";
import { useEntityCrud, type EntityCrud } from "../src/lib/entity-crud";

type Store = { readonly store_id: string; readonly name: string };
const LTT: Store = { store_id: "01M221BB8BB5SESQDB895SJHJS", name: "4P's Le Thanh Ton" };

/** A promise the test settles, so `saving` can be observed while it is true. */
function deferred() {
  let settle!: () => void;
  const promise = new Promise<void>((resolve) => {
    settle = resolve;
  });
  return { promise, settle };
}

function mount(options: {
  crud: EntityCrud<Store>;
  onSubmit: () => void;
  as?: "drawer" | "modal";
  dirty?: () => boolean;
}) {
  return render(() => (
    <FormPanel
      crud={options.crud}
      createTitle="Add store"
      editTitle="Edit store"
      submitLabel="Save"
      onSubmit={options.onSubmit}
      as={options.as}
      dirty={options.dirty}
    >
      <TextField label="Name" value="" onInput={() => {}} />
    </FormPanel>
  ));
}

describe("the form shell", () => {
  afterEach(cleanup);

  it("stays shut while nothing is being authored", () => {
    const crud = useEntityCrud<Store>();
    mount({ crud, onSubmit: () => {} });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("opens on create, titled for creating", () => {
    const crud = useEntityCrud<Store>();
    mount({ crud, onSubmit: () => {} });
    crud.create();
    expect(screen.getByRole("dialog")).toBeTruthy();
    expect(screen.getByText("Add store")).toBeTruthy();
  });

  it("opens on edit, titled for editing", () => {
    const crud = useEntityCrud<Store>();
    mount({ crud, onSubmit: () => {} });
    crud.edit(LTT);
    expect(screen.getByText("Edit store")).toBeTruthy();
  });

  // A destructive question belongs to `ConfirmDialog`. If the panel opened here too, an operator
  // would be asked to confirm a deletion with an edit form on top of the question.
  it("stays shut while a destructive action is being confirmed", () => {
    const crud = useEntityCrud<Store>();
    mount({ crud, onSubmit: () => {} });
    crud.confirm(LTT);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("submits on the footer button", () => {
    const crud = useEntityCrud<Store>();
    const onSubmit = vi.fn();
    mount({ crud, onSubmit });
    crud.create();

    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(onSubmit).toHaveBeenCalledOnce();
  });

  it("shows the refusal the lifecycle is carrying", async () => {
    const crud = useEntityCrud<Store>();
    mount({ crud, onSubmit: () => {} });
    crud.edit(LTT);

    await crud.run(() => Promise.reject(new Error("Name is already taken.")));

    expect(screen.getByRole("dialog")).toBeTruthy();
    expect(screen.getByText(/Name is already taken/)).toBeTruthy();
  });

  // A double-click on a slow save would otherwise send the write twice — and for a create, make two
  // records.
  it("disables both footer buttons while the save is in flight", async () => {
    const crud = useEntityCrud<Store>();
    mount({ crud, onSubmit: () => {} });
    crud.create();
    const inFlight = deferred();

    const running = crud.run(() => inFlight.promise);

    expect(screen.getByRole("button", { name: "Saving…" })).toHaveProperty("disabled", true);
    expect(screen.getByRole("button", { name: "Cancel" })).toHaveProperty("disabled", true);

    inFlight.settle();
    await running;
  });

  it("closes on cancel when there is nothing to lose", () => {
    const crud = useEntityCrud<Store>();
    mount({ crud, onSubmit: () => {} });
    crud.create();

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(crud.mode()).toBe("idle");
  });

  it("closes on Escape", () => {
    const crud = useEntityCrud<Store>();
    mount({ crud, onSubmit: () => {} });
    crud.create();

    fireEvent.keyDown(window, { key: "Escape" });

    expect(crud.mode()).toBe("idle");
  });

  it("asks before discarding unsaved input, and keeps the form when told to", () => {
    const crud = useEntityCrud<Store>();
    mount({ crud, onSubmit: () => {}, dirty: () => true });
    crud.create();

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(crud.mode()).toBe("creating");
    expect(screen.getByText("Discard your unsaved changes?")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Keep editing" }));

    expect(crud.mode()).toBe("creating");
    expect(screen.queryByText("Discard your unsaved changes?")).toBeNull();
  });

  it("discards when the operator confirms it", () => {
    const crud = useEntityCrud<Store>();
    mount({ crud, onSubmit: () => {}, dirty: () => true });
    crud.create();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    fireEvent.click(screen.getByRole("button", { name: "Discard" }));

    expect(crud.mode()).toBe("idle");
  });

  it("asks on Escape too, not only on cancel", () => {
    const crud = useEntityCrud<Store>();
    mount({ crud, onSubmit: () => {}, dirty: () => true });
    crud.create();

    fireEvent.keyDown(window, { key: "Escape" });

    expect(crud.mode()).toBe("creating");
    expect(screen.getByText("Discard your unsaved changes?")).toBeTruthy();
  });

  // The write has already gone to the server; closing would leave the operator unable to see how it
  // landed, and — on a refusal — would throw away the typing that is the only copy of their intent.
  it("cannot be closed while a save is in flight", async () => {
    const crud = useEntityCrud<Store>();
    mount({ crud, onSubmit: () => {} });
    crud.create();
    const inFlight = deferred();
    const running = crud.run(() => inFlight.promise);

    fireEvent.keyDown(window, { key: "Escape" });

    expect(crud.mode()).toBe("creating");

    inFlight.settle();
    await running;
  });

  it("renders as a modal when asked", () => {
    const crud = useEntityCrud<Store>();
    mount({ crud, onSubmit: () => {}, as: "modal" });
    crud.create();
    // Both shapes are `role="dialog"`; the modal is the one that centres rather than filling the
    // right edge, which is a class rather than a role.
    expect(screen.getByRole("dialog").className).toContain("max-w-lg");
  });

  it("renders as a drawer by default", () => {
    const crud = useEntityCrud<Store>();
    mount({ crud, onSubmit: () => {} });
    crud.create();
    expect(screen.getByRole("dialog").className).toContain("h-full");
  });
});
