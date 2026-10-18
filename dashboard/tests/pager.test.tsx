// The pager is a navigation landmark with a name of its own.
//
// A screen-reader user lists landmarks to move around a page. The pager's `<nav>` is named for what
// it is, "Pagination", from a translation key; the range text ("1–20 of 57") stays the visible
// status beside the buttons, since a landmark named after numbers that change with every page reads
// as a different landmark each time.

import { cleanup, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { Pager } from "../src/components/kit";

afterEach(cleanup);

describe("the pager", () => {
  it("is a navigation landmark named Pagination", () => {
    render(() => <Pager offset={0} limit={20} total={57} shown={20} onOffset={() => {}} />);
    const nav = screen.getByRole("navigation", { name: "Pagination" });
    expect(nav.textContent).toContain("1–20 of 57");
  });

  it("renders nothing when everything fits on one page", () => {
    render(() => <Pager offset={0} limit={20} total={5} shown={5} onOffset={() => {}} />);
    expect(screen.queryByRole("navigation")).toBeNull();
  });
});
