// The console's half of "the preview matches the till" (ADR-0209 decision 4): the designer's
// preview draws the committed fixture in the shape the till's own mount draws it in
// (`ui/unit/preview-matches.test.ts` writes it), regions as outlines with their contents left out.

import { afterEach, describe, expect, it } from "vitest";
import { render } from "solid-js/web";

import { FIXTURE_HOST, FIXTURE_WORDS, SHAPE_FILE, shapes } from "../../packages/package-renderer/fixtures/preview";
import type { ScreenNode } from "../../packages/package-renderer/src/components";
import { componentName } from "../src/screens/packages/screens/names";
import { PreviewTree } from "../src/screens/packages/screens/Preview";

const disposals: (() => void)[] = [];

afterEach(() => {
  disposals.splice(0).forEach((dispose) => dispose());
  document.body.innerHTML = "";
});

function draw(root: ScreenNode): Element {
  const target = document.createElement("div");
  document.body.append(target);
  disposals.push(render(() => PreviewTree({ root, words: FIXTURE_WORDS, host: FIXTURE_HOST }), target));
  return target;
}

describe("the preview matches the till", () => {
  it("draws the fixture in the shape the till draws it", async () => {
    await expect(shapes(draw)).toMatchFileSnapshot(SHAPE_FILE);
  });

  it("outlines a region and names it", () => {
    const target = draw({ component_id: "order_bill" });
    expect(target.querySelector('[data-region="order_bill"]')?.textContent).toBe(componentName("order_bill"));
  });
});
