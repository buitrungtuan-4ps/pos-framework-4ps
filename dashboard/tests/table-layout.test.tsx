// What a list looks like when the screen is too narrow for a table (V9, V18).
//
// Eight console tables clipped at phone width with no scroll container at all, and the fix that
// looks obvious — wrap it in `overflow-x-auto` — only makes the clipping swipeable. Below `md` the
// table becomes a list of cards, one per row, each a stack of label/value pairs taken from the
// column headers.
//
// The part worth pinning is that it is *either/or*. Rendering both behind a `hidden` class would
// put two copies of every cell in the document: duplicate ids, a screen reader walking the whole
// page twice, and — on the grids whose cells hold an input — two edit boxes for one value, of which
// the operator can see one. So these tests assert the absence of the other layout, not just the
// presence of the one asked for.
//
// `createIsWide` falls back to "wide" when `matchMedia` is missing, which is what jsdom gives you
// by default; the narrow cases here install a stub, and `restoreMatchMedia` takes it away again so
// one test's viewport cannot leak into the next.

import { cleanup, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { type Column, DataTable } from "../src/components/kit";

type Shop = { id: string; name: string; city: string };

const SHOPS: Shop[] = [
  { id: "s1", name: "Bến Thành", city: "Hồ Chí Minh" },
  { id: "s2", name: "Xuân Thuỷ", city: "Hà Nội" },
];

const COLUMNS: readonly Column<Shop>[] = [
  { key: "name", header: "Shop", cell: (row) => <span>{row.name}</span> },
  { key: "city", header: "City", cell: (row) => <span>{row.city}</span> },
];

/** Pins `matchMedia` to one answer for the life of a test. */
function setViewport(wide: boolean) {
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    writable: true,
    value: () => ({
      matches: wide,
      addEventListener: () => {},
      removeEventListener: () => {},
    }),
  });
}

function restoreMatchMedia() {
  // Back to jsdom's own state — absent — which is the "wide" fallback the component documents.
  Reflect.deleteProperty(window, "matchMedia");
}

afterEach(() => {
  cleanup();
  restoreMatchMedia();
});

describe("a list wider than the screen", () => {
  it("is a table when there is room for one", () => {
    setViewport(true);
    render(() => <DataTable columns={COLUMNS} rows={SHOPS} empty={<p>none</p>} />);

    expect(screen.getByRole("table")).toBeTruthy();
    expect(screen.queryByRole("list")).toBeNull();
    expect(screen.getByRole("columnheader", { name: "Shop" })).toBeTruthy();
  });

  it("becomes one card per row below md, and the table is gone rather than hidden", () => {
    setViewport(false);
    render(() => <DataTable columns={COLUMNS} rows={SHOPS} empty={<p>none</p>} />);

    expect(screen.queryByRole("table")).toBeNull();
    expect(screen.getAllByRole("listitem")).toHaveLength(SHOPS.length);
    // The header text is the card's label for that value, which is why `Column.header` is a plain
    // translated string and not a node.
    expect(screen.getAllByText("Shop")).toHaveLength(SHOPS.length);
    expect(screen.getByText("Bến Thành")).toBeTruthy();
    expect(screen.getByText("Hồ Chí Minh")).toBeTruthy();
  });

  it("falls back to the table when there is no matchMedia to ask", () => {
    // jsdom's default, and the direction that degrades to a scroll rather than to the wrong layout.
    expect(window.matchMedia).toBeUndefined();
    render(() => <DataTable columns={COLUMNS} rows={SHOPS} empty={<p>none</p>} />);

    expect(screen.getByRole("table")).toBeTruthy();
  });
});

describe("row density", () => {
  it("is comfortable unless a reference grid asks otherwise", () => {
    setViewport(true);
    render(() => <DataTable columns={COLUMNS} rows={SHOPS} empty={<p>none</p>} />);

    const cell = screen.getByText("Bến Thành").closest("td");
    expect(cell?.className).toContain("py-2");
  });

  it("tightens to compact, which is the only thing compact does", () => {
    setViewport(true);
    render(() => (
      <DataTable columns={COLUMNS} rows={SHOPS} empty={<p>none</p>} density="compact" />
    ));

    const cell = screen.getByText("Bến Thành").closest("td");
    expect(cell?.className).toContain("py-1");
    expect(cell?.className).not.toContain("py-2");
  });
});
