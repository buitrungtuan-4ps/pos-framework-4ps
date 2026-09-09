// Every loading placeholder in the console is a skeleton, and none of them went quiet (Stage 3b).
//
// The per-site swap is mechanical, which is exactly why it needs a check that is not: a reviewer
// reading fourteen near-identical diff hunks will not notice the one that dropped the label, and no
// gate would either — a `Skeleton` with no accessible name renders perfectly.
//
// This reads the source rather than rendering fourteen screens, which is a deliberate trade: the
// property is "no bare loading sentence survives where content is arriving, and every skeleton
// carries the words", and that is a property of the call sites. Rendering each screen would need
// fourteen sets of API mocks to assert something the call site already states plainly.
//
// The sources come through `import.meta.glob(… ?raw)` rather than `node:fs`. Vite resolves it at
// transform time and types it from `vite/client`, which this project already has — reaching for the
// Node API would mean adding `@types/node` to type three lines of a test.

import { describe, expect, it } from "vitest";

const sources: Record<string, string> = import.meta.glob("../src/**/*.tsx", {
  query: "?raw",
  import: "default",
  eager: true,
});

const entries = Object.entries(sources);

describe("the console's loading placeholders", () => {
  it("finds the source, so this suite cannot pass by looking at nothing", () => {
    expect(entries.length).toBeGreaterThan(20);
    expect(entries.filter(([, text]) => text.includes("<Skeleton")).length).toBeGreaterThanOrEqual(
      10,
    );
  });

  it("gives every skeleton an accessible label", () => {
    const unlabelled = entries.flatMap(([path, text]) =>
      [...text.matchAll(/<Skeleton\b[^/>]*\/?>/g)]
        .filter((match) => !match[0].includes("label="))
        .map(() => path),
    );
    expect(unlabelled).toEqual([]);
  });

  it("leaves no bare loading sentence where a block of content is arriving", () => {
    // One call site legitimately keeps the words: the context picker's create button, whose own
    // label becomes "Loading…" while a request is in flight. A button is not a placeholder for
    // content that is coming, so a skeleton there would be wrong rather than missing.
    const survivors = entries.flatMap(([path, text]) =>
      text
        .split("\n")
        .filter((line) => line.includes('t("common.loading")') && !line.includes("<Skeleton"))
        .map((line) => `${path}: ${line.trim()}`),
    );
    expect(survivors).toHaveLength(1);
    expect(survivors[0]).toContain("ContextPicker.tsx");
    expect(survivors[0]).toContain("busy()");
  });
});
