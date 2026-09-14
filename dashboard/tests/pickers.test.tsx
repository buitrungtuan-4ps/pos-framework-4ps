// The two searchable pickers, and the click that used to cost fifteen choices.
//
// `MultiSelectField` was a `<select multiple>`, and a native multi-select treats a plain click as
// "select only this" — adding is Ctrl/Cmd-click, which nothing on screen said. An operator building
// a modifier group of sixteen toppings lost the first fifteen on the sixteenth click, silently,
// with no undo. That is the behaviour pinned first below, because it is the one a refactor could
// quietly reintroduce by reaching for `selectedOptions` again.
//
// The rest is the property that made both controls worth writing: **a list you can type into.**
// Both were fed the tenant's whole item master, and a native select's only search is type-ahead on
// the first character — which, for a catalogue of Vietnamese names, is not a search.

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ComboboxField, MultiComboboxField } from "../src/components/ui";

// Deliberately non-ASCII and deliberately sharing prefixes: the collation and the substring match
// are the two things a picker over this catalogue has to get right.
const ITEMS = [
  { value: "i1", label: "Bánh mì thịt nguội" },
  { value: "i2", label: "Bún bò Huế" },
  { value: "i3", label: "Margherita" },
  // The bilingual case (ADR-0074): the label is the fallback name the catalogue was authored
  // under, and the operator at the console is typing the other one.
  { value: "i4", label: "Marinara", keywords: ["Sốt cà chua tỏi"] },
];

const LABELS = {
  searchLabel: "Search items",
  emptyLabel: "Nothing matches that search",
  removeLabel: "Remove from the selection",
};

afterEach(cleanup);

describe("the single-choice picker", () => {
  function mount(initial = "") {
    const [value, setValue] = createSignal(initial);
    const onChange = vi.fn((next: string) => setValue(next));
    render(() => (
      <ComboboxField
        label="Item"
        value={value()}
        options={ITEMS}
        onChange={onChange}
        placeholder="Choose an item"
        searchLabel={LABELS.searchLabel}
        emptyLabel={LABELS.emptyLabel}
      />
    ));
    return { onChange, value };
  }

  const trigger = () => screen.getByRole("button", { name: "Item" });

  it("reads as the placeholder until something is chosen, then as the choice", () => {
    mount("i3");
    expect(trigger().textContent).toBe("Margherita");
  });

  it("issues no list until it is opened, so a form of ten pickers renders ten buttons", () => {
    // The reason this is not a styling detail: the old control rendered every option into the DOM
    // on every render, for every picker on the screen, whether or not anyone opened one.
    mount();
    expect(screen.queryByRole("listbox")).toBeNull();
    fireEvent.click(trigger());
    expect(screen.getAllByRole("option")).toHaveLength(ITEMS.length);
  });

  it("filters on a fragment from the middle of a name, which type-ahead cannot do", () => {
    mount();
    fireEvent.click(trigger());
    fireEvent.input(screen.getByRole("combobox"), { target: { value: "bò" } });
    expect(screen.getAllByRole("option").map((node) => node.textContent)).toEqual(["Bún bò Huế"]);
  });

  it("finds an item by a name in another locale, which is how half this catalogue is typed", () => {
    // A tenant authors items in English and its staff search in Vietnamese, or the reverse. The
    // per-locale names are already on the wire with every item, so the local filter can answer
    // this without a round-trip — and a filter that only saw `name` would return nothing at all.
    mount();
    fireEvent.click(trigger());
    fireEvent.input(screen.getByRole("combobox"), { target: { value: "cà chua" } });
    expect(screen.getAllByRole("option").map((node) => node.textContent)).toEqual(["Marinara"]);
  });

  it("says so rather than showing an empty box when nothing matches", () => {
    mount();
    fireEvent.click(trigger());
    fireEvent.input(screen.getByRole("combobox"), { target: { value: "phở" } });
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(screen.getByText(LABELS.emptyLabel)).toBeTruthy();
  });

  it("chooses, closes, and forgets the query", () => {
    const { onChange } = mount();
    fireEvent.click(trigger());
    fireEvent.input(screen.getByRole("combobox"), { target: { value: "mari" } });
    fireEvent.click(screen.getByRole("option", { name: "Marinara" }));
    expect(onChange).toHaveBeenCalledWith("i4");
    expect(screen.queryByRole("listbox")).toBeNull();
    // Re-opening starts clean: a stale query is a picker that appears to have lost most of the
    // catalogue.
    fireEvent.click(trigger());
    expect(screen.getAllByRole("option")).toHaveLength(ITEMS.length);
  });

  it("takes the keyboard: down to the second row, Enter to choose it, and scrolls active item into view", () => {
    const { onChange } = mount();
    fireEvent.click(trigger());
    const search = screen.getByRole("combobox");
    const options = screen.getAllByRole("option");
    const scrollSpy = vi.fn();
    if (options[1]) {
      options[1].scrollIntoView = scrollSpy;
    }

    fireEvent.keyDown(search, { key: "ArrowDown" });
    expect(scrollSpy).toHaveBeenCalledWith({ block: "nearest" });

    fireEvent.keyDown(search, { key: "Enter" });
    expect(onChange).toHaveBeenCalledWith("i2");
  });

  it("hands filtering to the caller when the list is served, and filters nothing itself", () => {
    // A paged endpoint has already applied the query. Filtering the returned page again would hide
    // rows the server chose to return, and the list would disagree with the count beside it.
    const onSearch = vi.fn();
    render(() => (
      <ComboboxField
        label="Served"
        value=""
        options={ITEMS}
        onChange={vi.fn()}
        placeholder="Choose an item"
        searchLabel={LABELS.searchLabel}
        emptyLabel={LABELS.emptyLabel}
        onSearch={onSearch}
      />
    ));
    fireEvent.click(screen.getByRole("button", { name: "Served" }));
    fireEvent.input(screen.getByRole("combobox"), { target: { value: "phở" } });
    expect(onSearch).toHaveBeenCalledWith("phở");
    expect(screen.getAllByRole("option")).toHaveLength(ITEMS.length);
  });
});

describe("the multi-choice picker", () => {
  function mount(initial: string[] = []) {
    const [values, setValues] = createSignal<readonly string[]>(initial);
    const onChange = vi.fn((next: string[]) => setValues(next));
    render(() => (
      <MultiComboboxField
        label="Members"
        values={values()}
        options={ITEMS}
        onChange={onChange}
        searchLabel={LABELS.searchLabel}
        emptyLabel={LABELS.emptyLabel}
        removeLabel={LABELS.removeLabel}
      />
    ));
    return { onChange, values };
  }

  it("adds on a plain click instead of replacing — the defect that retired the old control", () => {
    const { values } = mount(["i1", "i2", "i3"]);
    fireEvent.click(screen.getByRole("option", { name: /Marinara/ }));
    expect([...values()]).toEqual(["i1", "i2", "i3", "i4"]);
  });

  it("removes on a second click, so the same gesture undoes itself", () => {
    const { values } = mount(["i1", "i2"]);
    fireEvent.click(screen.getByRole("option", { name: /Bún bò Huế/ }));
    expect([...values()]).toEqual(["i1"]);
  });

  it("marks what is chosen, so the list itself answers 'is this one in'", () => {
    mount(["i2"]);
    const chosen = screen.getByRole("option", { name: /Bún bò Huế/ });
    expect(chosen.getAttribute("aria-selected")).toBe("true");
    expect(
      screen.getByRole("option", { name: /Margherita/ }).getAttribute("aria-selected"),
    ).toBe("false");
  });

  it("keeps the whole selection on screen while a query hides most of the list", () => {
    // The reason the chips exist. With three chosen and a query that matches one, the other two
    // leave the list — and if the chips went with them, "what have I picked" would be a question
    // you answer by clearing the search.
    mount(["i1", "i2", "i3"]);
    fireEvent.input(screen.getByRole("combobox"), { target: { value: "margh" } });
    expect(screen.getAllByRole("option")).toHaveLength(1);
    for (const name of ["Bánh mì thịt nguội", "Bún bò Huế", "Margherita"]) {
      expect(screen.getByRole("button", { name: `${LABELS.removeLabel}: ${name}` })).toBeTruthy();
    }
  });

  it("removes exactly one choice from its chip, and leaves the rest", () => {
    const { values } = mount(["i1", "i2", "i3"]);
    fireEvent.click(
      screen.getByRole("button", { name: `${LABELS.removeLabel}: Bún bò Huế` }),
    );
    expect([...values()]).toEqual(["i1", "i3"]);
  });

  it("toggles the active row on Enter, so the whole control works without a mouse, and scrolls active item into view", () => {
    const { values } = mount([]);
    const search = screen.getByRole("combobox");
    const options = screen.getAllByRole("option");
    const scrollSpy = vi.fn();
    if (options[2]) {
      options[2].scrollIntoView = scrollSpy;
    }

    fireEvent.keyDown(search, { key: "ArrowDown" });
    fireEvent.keyDown(search, { key: "ArrowDown" });
    expect(scrollSpy).toHaveBeenCalledWith({ block: "nearest" });

    fireEvent.keyDown(search, { key: "Enter" });
    expect([...values()]).toEqual(["i3"]);
  });
});
