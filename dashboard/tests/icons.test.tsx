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

import { Icon, ICON_NAMES } from "../src/components/icons";
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
  // Until Wave 4 the set was exactly one glyph per nav screen, and this checked that correspondence
  // in both directions. PR-2 added three glyphs no screen owns — the nav toggle, the palette's
  // search affordance and the notification bell, each of which had been an emoji (finding V7) and so
  // rendered as a different picture on every operating system. The invariant that still holds, and
  // the one worth gating, is the reason the old one was written: **no glyph ships unused**. So a
  // glyph counts as drawn if a screen declares it *or* any component asks for it by name.
  const drawn = new Set<string>((Object.keys(SCREENS) as ScreenId[]).map((id) => SCREENS[id].icon));
  const components: Record<string, string> = import.meta.glob("../src/**/*.tsx", {
    query: "?raw",
    import: "default",
    eager: true,
  });
  for (const [path, text] of Object.entries(components)) {
    if (path.endsWith("components/icons.tsx")) {
      continue;
    }
    for (const match of text.matchAll(/<Icon\s+name="([a-z0-9-]+)"/g)) {
      drawn.add(match[1] as string);
    }
  }

  it("is drawn somewhere, so none is dead weight in the bundle", () => {
    const orphans = ICON_NAMES.filter((name) => !drawn.has(name));
    expect(orphans).toEqual([]);
  });

  it("exists for every icon a screen declares", () => {
    // The other direction, now stated over the screens alone: a nav entry naming a glyph the set
    // does not carry is a blank square in the sidebar, which `ScreenId`'s type cannot catch on its
    // own once the set and the screens are no longer one list.
    const missing = (Object.keys(SCREENS) as ScreenId[])
      .map((id) => SCREENS[id].icon)
      .filter((name) => !(ICON_NAMES as readonly string[]).includes(name));
    expect(missing).toEqual([]);
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
