// No list screen renders an unbounded number of rows (Wave 3 · Stage 3c).
//
// # What was measured, and what it decided
//
// "Virtualise the tables" sat in the roadmap as planned work on the strength of `DataTable`'s own
// docstring — "right-sized for tens to hundreds of rows" — which nobody had measured. So it was
// measured (`bench/measure.mjs`, Chromium): an unpaged five-column table paints
//
//   100 rows → 36 ms   1,000 → 161 ms   2,000 → 322 ms   5,000 → 1.6 s   10,000 → 4.0 s
//
// and 10,000 rows behind a page size of 25 → 37 ms, indistinguishable from 100 rows unpaged.
//
// A page size is therefore a *complete* answer to volume in this component, and virtualisation would
// be a large change to the one table every list screen renders in order to solve a problem the
// constant already solves at a hundred times the expected load. It is not built, and this is the
// record of why.
//
// # Why a test and not just a note
//
// Eight tables had no bound at all — not because anybody chose that, but because `pageSize` is
// optional and easy to forget. The next screen will forget it too. This fails when a `DataTable`
// appears with neither a client page size nor server paging, which is the only way the measurement
// above stops applying.

import { describe, expect, it } from "vitest";

const sources: Record<string, string> = import.meta.glob("../src/**/*.tsx", {
  query: "?raw",
  import: "default",
  eager: true,
});

/** Every `<DataTable …>` opening tag, brace-aware so a `{…}` prop containing `>` cannot end it. */
function dataTableTags(text: string): string[] {
  const tags: string[] = [];
  for (const match of text.matchAll(/<DataTable\b/g)) {
    let depth = 0;
    let index = match.index;
    while (index < text.length) {
      const char = text[index];
      if (char === "{") {
        depth += 1;
      } else if (char === "}") {
        depth -= 1;
      } else if (char === ">" && depth === 0) {
        break;
      }
      index += 1;
    }
    tags.push(text.slice(match.index, index));
  }
  return tags;
}

const tables = Object.entries(sources).flatMap(([path, text]) =>
  dataTableTags(text).map((tag) => ({ path, tag })),
);

describe("every table in the console", () => {
  it("is found, so this suite cannot pass by looking at nothing", () => {
    // The console had 38 at the time of writing; the floor is deliberately loose so adding screens
    // does not fail the suite, while deleting the glob does.
    expect(tables.length).toBeGreaterThanOrEqual(30);
  });

  it("has a ceiling on how many rows it can render at once", () => {
    const unbounded = tables
      .filter(({ tag }) => !tag.includes("pageSize") && !tag.includes("serverTotal"))
      .map(({ path }) => path);
    expect(unbounded).toEqual([]);
  });

  it("pages on the server or slices on the client, never claims to do both differently", () => {
    // ADR-0098: in server mode `rows` is one page already, so `pageSize` must be the limit that was
    // asked for and `onPage` must be there to ask for the next one. A `serverTotal` without an
    // `onPage` renders a pager whose buttons do nothing.
    const halfServer = tables
      .filter(({ tag }) => tag.includes("serverTotal") !== tag.includes("onPage"))
      .map(({ path }) => path);
    expect(halfServer).toEqual([]);
  });
});
