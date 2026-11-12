// The screen designer writes the draft's screens from clicks alone: a screen added, a component
// added from the palette, a property given, the home a till opens after sign-in chosen (ADR-0210);
// a refusal outlines the node it names; and the preview draws the chosen variant with the till's
// renderer as it is edited (ADR-0209).

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { PackageRefusal } from "../src/api/schemas";
import en from "../src/i18n/en.json";
import { blankDraft, type PackageDraft } from "../src/lib/package-draft";
import { ScreensPart } from "../src/screens/packages/screens/ScreensPart";

vi.mock("../src/api/client", () => ({
  api: { getTranslations: () => Promise.resolve({ value: { "welcome.title": { en: "Welcome" } }, etag: null }) },
}));

const messages = en as Record<string, string>;

function mount(refusals: readonly PackageRefusal[] = [], initial: PackageDraft = blankDraft("May")) {
  const [draft, setDraft] = createSignal<PackageDraft>(initial);
  render(() => <ScreensPart draft={draft} change={(next) => setDraft(next(draft()))} refusals={refusals} />);
  return draft;
}

describe("the screen designer", () => {
  afterEach(cleanup);

  it("adds a screen, a heading from the palette and the heading's title", () => {
    const draft = mount();
    fireEvent.input(document.querySelector('[data-step="setScreenId"]')!, { target: { value: "welcome" } });
    fireEvent.click(screen.getByRole("button", { name: messages["designer.screens.addScreen"]! }));
    fireEvent.click(screen.getByRole("button", { name: "Add Page heading" }));
    fireEvent.input(screen.getByLabelText(/^Title \(required\)/), { target: { value: "welcome.title" } });
    expect(draft().screens).toEqual([
      {
        selector: {},
        body: {
          screen_id: "welcome",
          root: {
            component_id: "screen_frame",
            children: [{ component_id: "page_header", properties: { title_key: "welcome.title" } }],
          },
        },
      },
    ]);
  });

  it("previews the screen as the till draws it, regions outlined", async () => {
    const initial: PackageDraft = {
      ...blankDraft("May"),
      screens: [{ selector: {}, body: { screen_id: "welcome", root: { component_id: "screen_frame", children: [{ component_id: "page_header" }, { component_id: "order_bill" }] } } }],
    };
    mount([], initial);
    const preview = document.querySelector('[data-outcome="screen-preview"]')!;
    expect(preview.querySelector('[data-region="order_bill"]')).toBeTruthy();
    fireEvent.click(document.querySelector('[data-node-path="0"] > button')!);
    fireEvent.input(screen.getByLabelText(/^Title \(required\)/), { target: { value: "welcome.title" } });
    await waitFor(() => expect(document.querySelector('[data-outcome="screen-preview"] h1')?.textContent).toBe("Welcome"));
  });

  it("names the screen a till opens after sign-in, and can go back to the release's own", () => {
    const initial: PackageDraft = {
      ...blankDraft("May"),
      screens: [{ selector: {}, body: { screen_id: "welcome", root: { component_id: "screen_frame" } } }],
    };
    const draft = mount([], initial);
    const home = screen.getByLabelText(new RegExp(`^${messages["designer.screens.home"]!}`)) as HTMLSelectElement;
    fireEvent.change(home, { target: { value: "welcome" } });
    expect(draft().home_screens).toEqual([{ selector: {}, body: { screen_id: "welcome" } }]);
    fireEvent.change(home, { target: { value: "" } });
    expect(draft().home_screens).toEqual([]);
  });

  it("outlines the node a refusal names", () => {
    const initial: PackageDraft = {
      ...blankDraft("May"),
      screens: [{ selector: {}, body: { screen_id: "welcome", root: { component_id: "screen_frame", children: [{ component_id: "page_header" }] } } }],
    };
    mount([{ field: "screens", reason: "MISSING_PROPERTY", message: "", screen_index: 0, node_index: 1, name: "title_key" }], initial);
    expect(screen.getByText("Screen welcome, part 2: title_key is required.")).toBeTruthy();
    expect(document.querySelector('[data-node-path="0"]')?.className).toContain("border-danger");
  });
});

describe("the tap budget in the screen designer", () => {
  afterEach(cleanup);

  it("notes an act over its budget beside the node offering it, once per role and cost", () => {
    const initial: PackageDraft = {
      ...blankDraft("May"),
      screens: [
        {
          selector: {},
          body: {
            screen_id: "far",
            root: {
              component_id: "stack",
              children: [{ component_id: "action_button", properties: { label_key: "welcome.title", action: { action_id: "billing.check.print", params: {} } } }],
            },
          },
        },
      ],
    };
    const warning = (device_class: string) => ({
      reason: "TAP_BUDGET",
      device_class,
      action_id: "billing.check.print",
      screen_index: 0,
      node_index: 1,
      taps: 3,
    });
    const [draft, setDraft] = createSignal<PackageDraft>(initial);
    render(() => (
      <ScreensPart
        draft={draft}
        change={(next) => setDraft(next(draft()))}
        refusals={[]}
        warnings={[warning("DEVICE_CLASS_TERMINAL"), warning("DEVICE_CLASS_PHONE")]}
      />
    ));
    const notes = document.querySelectorAll('[data-node-path="0"] [data-outcome="tap-warning"]');
    expect(notes).toHaveLength(1);
    expect(notes[0]?.textContent).toContain("3 taps");
    expect(document.querySelectorAll('[data-node-path=""] > [data-outcome="tap-warning"]')).toHaveLength(0);
  });
});
