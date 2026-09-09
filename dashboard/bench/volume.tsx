// A measurement, not a screen: how long the console's table takes to mount N rows.
//
// # Why this exists
//
// `DataTable`'s own docstring claimed it was "right-sized for the admin lists' volumes — tens to
// hundreds of rows per tenant". That was an assertion, not a measurement, and the roadmap carried
// "virtualise the tables" as planned work on the strength of it. Virtualisation is a large, risky
// change to the one component every list screen renders; deciding to build it — or not to — on a
// guess is how a console acquires machinery it does not need.
//
// So this mounts the real component with synthetic rows and reports the paint time. It is served by
// the dev server at `/bench/volume.html?rows=N&page=M`; `bench/measure.mjs` drives it in Chromium
// and prints a table of numbers.
//
// Deliberately outside `src/`: it carries bare English labels, and `src/` is what the
// no-hardcoded-strings gate walks. It is in `tsconfig.json`'s include list, so it still type-checks
// against the component it measures — a bench that has drifted from the props it passes measures
// nothing.

import { render } from "solid-js/web";

import { DataTable } from "../src/components/kit";
import "../src/app.css";

type Row = {
  readonly id: string;
  readonly name: string;
  readonly status: string;
  readonly count: number;
  readonly when: string;
};

/**
 * Rows shaped like a real admin list: an id, a name, a status, a number and a date, which is what
 * the five-column screens (Stores, Devices, Employees, Items) actually render.
 *
 * Deterministic, so two runs are comparable. Randomness here would make the numbers noise.
 */
function rows(count: number): Row[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `01M22190WCY5PS7KCA7ET7H${String(index).padStart(3, "0")}`,
    name: `Store ${index} — Ho Chi Minh City`,
    status: index % 7 === 0 ? "archived" : "active",
    count: index * 3,
    when: `2026-09-${String((index % 28) + 1).padStart(2, "0")}`,
  }));
}

const COLUMNS = [
  { key: "name", header: "Name", cell: (row: Row) => row.name },
  { key: "status", header: "Status", cell: (row: Row) => row.status },
  { key: "count", header: "Count", cell: (row: Row) => String(row.count) },
  { key: "when", header: "Updated", cell: (row: Row) => row.when },
  { key: "id", header: "Identifier", cell: (row: Row) => row.id },
] as const;

const params = new URLSearchParams(window.location.search);
const rowCount = Number(params.get("rows") ?? "100");
const pageSize = Number(params.get("page") ?? "0");

const data = rows(rowCount);
const root = document.getElementById("root");

if (root === null) {
  throw new Error("bench: no #root to mount into");
}

// The clock starts before `render` and stops after the browser has had a frame to paint: two
// `requestAnimationFrame` ticks, because the first fires before the paint that follows it. Timing
// `render` alone would measure Solid's work and miss the layout and paint the operator waits for,
// which on a wide table is most of it.
const started = performance.now();
render(
  () => (
    <DataTable
      columns={COLUMNS}
      rows={data}
      empty={<p>nothing</p>}
      {...(pageSize > 0 ? { pageSize } : {})}
    />
  ),
  root,
);
requestAnimationFrame(() =>
  requestAnimationFrame(() => {
    const elapsed = performance.now() - started;
    const out = document.createElement("pre");
    out.id = "result";
    out.textContent = JSON.stringify({
      rows: rowCount,
      pageSize,
      renderedRows: document.querySelectorAll("tbody tr").length,
      ms: Math.round(elapsed * 10) / 10,
    });
    document.body.append(out);
  }),
);
