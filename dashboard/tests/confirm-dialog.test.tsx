// The typed-name guard on a destructive confirmation.
//
// Every irreversible console action — retiring a till, bumping a store's lease, archiving a store —
// asks the operator to type the entity's name before the confirm button enables. That guard was
// bypassable: `typed` lives in the component body and the component stays mounted for the life of
// the screen, so typing the name, cancelling, and reopening the dialog found the button already
// enabled. Fixed in #251 with a `createEffect` that clears on open; these tests are what keeps it
// fixed, because nothing in the build could tell that it had regressed.

import { createSignal } from "solid-js";
import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ConfirmDialog } from "../src/components/kit";

const TILL = "TILL-02";

/** The dialog as a screen uses it: mounted permanently, opened and closed by a signal. */
function mountDialog(options: { typeToConfirm?: string } = {}) {
  const [open, setOpen] = createSignal(true);
  const onConfirm = vi.fn();
  const utils = render(() => (
    <ConfirmDialog
      open={open()}
      title="Retire this till"
      message="The till stops working immediately."
      confirmLabel="Retire"
      cancelLabel="Cancel"
      closeLabel="Close"
      typeToConfirm={options.typeToConfirm}
      typePrompt="Type the till name to confirm"
      onConfirm={onConfirm}
      onCancel={() => setOpen(false)}
    />
  ));
  return { ...utils, open, setOpen, onConfirm };
}

const confirmButton = () => screen.getByRole("button", { name: "Retire" }) as HTMLButtonElement;
const nameField = () => screen.getByLabelText("Type the till name to confirm") as HTMLInputElement;

afterEach(cleanup);

describe("a destructive confirmation", () => {
  it("confirms on one click when no name is demanded", () => {
    const { onConfirm } = mountDialog();
    expect(confirmButton().disabled).toBe(false);
    fireEvent.click(confirmButton());
    expect(onConfirm).toHaveBeenCalledOnce();
  });

  it("holds the confirm button until the name matches", () => {
    mountDialog({ typeToConfirm: TILL });
    expect(confirmButton().disabled).toBe(true);

    fireEvent.input(nameField(), { target: { value: "TILL-0" } });
    expect(confirmButton().disabled).toBe(true);

    fireEvent.input(nameField(), { target: { value: TILL } });
    expect(confirmButton().disabled).toBe(false);
  });

  it("accepts a name typed with stray whitespace", () => {
    mountDialog({ typeToConfirm: TILL });
    fireEvent.input(nameField(), { target: { value: `  ${TILL} ` } });
    expect(confirmButton().disabled).toBe(false);
  });

  // The regression from #251, in the shape it actually took.
  it("demands the name again after a cancel and reopen", () => {
    const { setOpen } = mountDialog({ typeToConfirm: TILL });
    fireEvent.input(nameField(), { target: { value: TILL } });
    expect(confirmButton().disabled).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    setOpen(true);

    expect(confirmButton().disabled).toBe(true);
  });
});
