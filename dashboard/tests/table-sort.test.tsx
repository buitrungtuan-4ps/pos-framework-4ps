// How a console table orders text when a header is pressed: the way it is read, not code point by
// code point. "Table 2" comes before "Table 10", and case and accents do not split names that read
// the same, so "pho" sits beside "Phở" and the two keep the order they came in.

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { type Column, DataTable } from "../src/components/kit";

type Row = { id: string; name: string };

const ROWS: Row[] = [
  { id: "r1", name: "Table 10" },
  { id: "r2", name: "Phở" },
  { id: "r3", name: "table 3" },
  { id: "r4", name: "Table 2" },
  { id: "r5", name: "pho" },
];

const COLUMNS: readonly Column<Row>[] = [
  {
    key: "name",
    header: "Name",
    cell: (row) => <span>{row.name}</span>,
    sortValue: (row) => row.name,
  },
];

/** The names in the order the table draws its rows, header excluded. */
function drawnNames(): string[] {
  return screen
    .getAllByRole("row")
    .slice(1)
    .map((row) => row.textContent ?? "");
}

afterEach(cleanup);

describe("sorting a text column", () => {
  it("puts numbers in order and keeps names that read the same together", () => {
    render(() => <DataTable columns={COLUMNS} rows={ROWS} empty={<p>none</p>} />);
    fireEvent.click(screen.getByRole("button", { name: /Name/ }));
    expect(drawnNames()).toEqual(["Phở", "pho", "Table 2", "table 3", "Table 10"]);
  });

  it("reverses the same order on a second press", () => {
    render(() => <DataTable columns={COLUMNS} rows={ROWS} empty={<p>none</p>} />);
    const header = screen.getByRole("button", { name: /Name/ });
    fireEvent.click(header);
    fireEvent.click(header);
    expect(drawnNames()).toEqual(["Table 10", "table 3", "Table 2", "Phở", "pho"]);
  });
});
