// The nav's icons (Wave 3 · Stage 6).
//
// # What is already guaranteed, and therefore not tested here
//
// That every screen has an icon, and that the name resolves to a glyph, are both compile-time
// facts: `Screen.icon` is a required `IconName`, and `GLYPHS` is a `Record<IconName, …>`. A runtime
// test of either would assert what `tsc` already refuses to build without, which is a test that can
// only ever pass.
//
// # What is not guaranteed
//
// Three things, and each has a way of going wrong that nothing else would catch:
//
// 1. **The icons stay silent.** Every one rides beside a visible label, so the label is the
//    accessible name and the icon is decoration. Give a glyph a `<title>` — the obvious "helpful"
//    change — and every nav entry announces its name twice. That is a regression a sighted reviewer
//    cannot see.
// 2. **No glyph is orphaned.** The geometry is vendored source in the shell bundle, around 350
//    bytes raw per glyph on every first visit. A glyph nobody draws is that cost with no benefit,
//    and deleting a screen — or, as when the nav group headings gave up their icons, a whole class
//    of caller — would leave one behind silently. This check is what turned that into five
//    deletions rather than five orphans.
// 3. **No two glyphs share geometry.** Two names pointing at identical paths means a transcription
//    error — a copy-paste that took the wrong source — which reads as a plausible icon in the wrong
//    place rather than as a broken one.

import { render, screen } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";

import { Icon, ICON_NAMES, type IconName } from "../src/components/icons";
import { SCREENS, type ScreenId } from "../src/state/screens";

const source: string = Object.values(
  import.meta.glob("../src/components/icons.tsx", {
    query: "?raw",
    import: "default",
    eager: true,
  }),
)[0] as string;

describe("an icon", () => {
  it("renders geometry, so the vendored paths reach the DOM", () => {
    const { container } = render(() => <Icon name="store" />);
    const svg = container.querySelector("svg");
    expect(svg).toBeTruthy();
    expect((svg as SVGElement).querySelectorAll("path, line, circle, rect").length).toBeGreaterThan(
      0,
    );
  });

  it("is hidden from assistive technology, and adds nothing to a label beside it", () => {
    render(() => (
      <button type="button">
        <Icon name="store" />
        Stores
      </button>
    ));
    // Not "Stores Stores", and not "store icon Stores": the label is the accessible name, alone.
    const button = screen.getByRole("button", { name: "Stores" });
    expect(button.querySelector("svg")?.getAttribute("aria-hidden")).toBe("true");
  });

  it("carries no text node of its own, in any glyph", () => {
    // `<title>` and `<desc>` are the two SVG elements that would make a glyph speak. Checked over
    // the source rather than by rendering all 36, because the claim is about the file.
    expect(source).not.toMatch(/<title|<desc/);
  });
});

describe("every vendored glyph", () => {
  const used = new Set<IconName>(
    (Object.keys(SCREENS) as ScreenId[]).map((id) => SCREENS[id].icon),
  );

  it("is drawn by a screen, so none is dead weight in the first paint", () => {
    const orphans = ICON_NAMES.filter((name) => !used.has(name));
    expect(orphans).toEqual([]);
  });

  it("accounts for every icon the console asks for", () => {
    // The other direction. Both hold today because the set is exactly one glyph per screen, and the
    // point of checking both is that adding a glyph "for later" and forgetting to use it costs
    // bundle bytes.
    expect(used.size).toBe(ICON_NAMES.length);
  });
});

describe("no two glyphs", () => {
  it("share geometry, which would mean a transcription took the wrong source", () => {
    const bodies = new Map<string, string[]>();
    for (const match of source.matchAll(
      /"([a-z0-9-]+)": \(\) => \(\n {4}<>\n([\s\S]*?)\n {4}<\/>/g,
    )) {
      const name = match[1] as string;
      const body = (match[2] as string).replace(/\s+/g, " ").trim();
      bodies.set(body, [...(bodies.get(body) ?? []), name]);
    }
    expect(bodies.size).toBe(ICON_NAMES.length);
    const duplicated = [...bodies.values()].filter((names) => names.length > 1);
    expect(duplicated).toEqual([]);
  });
});
