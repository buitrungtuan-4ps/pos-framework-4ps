// The dry run in front of a publish (Wave 4 · PR-8, finding F11).
//
// Every publish bar but campaigns committed blind: the operator pressed Publish and found out
// afterwards, from the shop, whether the document was what they meant. `PublishBar` now takes a
// loader and grows a **Preview changes** button beside Publish. What is worth pinning is not the
// markup but the four answers the dialog has to give correctly, because each of them is one an
// operator would act on: what would change, that nothing would change, that this shop has nothing
// published yet, and — the one a screen gets wrong first — that a loader answering `null` must not
// open a dialog at all.

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import { PublishBar } from "../src/components/kit";
import type { NodePreview } from "../src/api/types";

const PREVIEW: NodePreview = {
  node: "tax",
  from_version_id: "01J000000000000000000000AA",
  diff: { tax: { rates: [{ code: "VAT", percent: 8 }] } },
  unchanged: false,
};

function mountBar(load: () => Promise<NodePreview | null>) {
  const onPublish = vi.fn();
  const utils = render(() => (
    <PublishBar
      label="Tax rates"
      publishedAtMs={1_000}
      describe={() => "On this store."}
      publishLabel="Publish"
      preview={load}
      onPublish={onPublish}
    />
  ));
  return { ...utils, onPublish };
}

afterEach(cleanup);

describe("previewing a publish", () => {
  it("shows the document the publish would write", async () => {
    mountBar(() => Promise.resolve(PREVIEW));
    fireEvent.click(screen.getByRole("button", { name: /^Preview/ }));

    await waitFor(() => expect(screen.getByRole("dialog")).toBeTruthy());
    const dialog = screen.getByRole("dialog");
    // The merge patch itself, not a summary of it: what the shop receives is the thing worth
    // checking, and a summary would be the console's opinion of it.
    expect(dialog.textContent).toContain("VAT");
    expect(dialog.textContent).toContain("01J000000000000000000000AA");
  });

  it("says plainly when nothing would change", async () => {
    mountBar(() =>
      Promise.resolve({ ...PREVIEW, diff: {}, unchanged: true } satisfies NodePreview),
    );
    fireEvent.click(screen.getByRole("button", { name: /^Preview/ }));

    await waitFor(() => expect(screen.getByRole("dialog")).toBeTruthy());
    // The case that stops the second press of Publish the run found somebody making.
    expect(screen.getByRole("dialog").textContent).toContain("Nothing would change");
  });

  it("says the shop has nothing published rather than naming a version it does not have", async () => {
    mountBar(() =>
      Promise.resolve({ ...PREVIEW, from_version_id: null } satisfies NodePreview),
    );
    fireEvent.click(screen.getByRole("button", { name: /^Preview/ }));

    await waitFor(() => expect(screen.getByRole("dialog")).toBeTruthy());
    expect(screen.getByRole("dialog").textContent).toContain("no published configuration yet");
  });

  it("opens nothing when there is nothing to preview", async () => {
    // What a screen hands back with no store chosen, or with its own picker still empty. A dialog
    // here would be an empty box the operator has to dismiss to learn nothing.
    const load = vi.fn(() => Promise.resolve(null));
    mountBar(load);
    fireEvent.click(screen.getByRole("button", { name: /^Preview/ }));

    await waitFor(() => expect(load).toHaveBeenCalled());
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("publishes from inside the dialog, so reading the diff and saying yes is one gesture", async () => {
    const { onPublish } = mountBar(() => Promise.resolve(PREVIEW));
    fireEvent.click(screen.getByRole("button", { name: /^Preview/ }));
    await waitFor(() => expect(screen.getByRole("dialog")).toBeTruthy());

    // Two controls read "Publish" once the dialog is open — the bar's and the dialog's. The
    // dialog's is the one inside the dialog, and taking it must both publish and close.
    const inDialog = screen
      .getByRole("dialog")
      .querySelectorAll<HTMLButtonElement>("button");
    const publish = [...inDialog].find((button) => button.textContent === "Publish");
    expect(publish).toBeTruthy();
    fireEvent.click(publish as HTMLButtonElement);

    expect(onPublish).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("offers no preview at all when a bar was given no loader", () => {
    render(() => (
      <PublishBar
        label="Store profile"
        publishedAtMs={1_000}
        describe={() => "On this store."}
        publishLabel="Publish"
        onPublish={vi.fn()}
      />
    ));
    // `locale` and `store_profile` are per-store by definition, so the cloud has no tenant-wide
    // document to compile for them. A button that always failed would be worse than none.
    expect(screen.queryByRole("button", { name: /^Preview/ })).toBeNull();
  });
});
