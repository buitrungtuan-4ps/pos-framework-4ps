// A loading placeholder must not go silent when it stops being a sentence (Wave 3 · Stage 3b).
//
// Thirteen places answered a pending read with `<p>Loading…</p>` — a line of text where a table, a
// card or a whole screen was about to appear, so the page jumped twice: once when the sentence
// replaced nothing, and again when the real content replaced the sentence and pushed everything
// below it down.
//
// The tempting fix is to swap the sentence for grey bars. That removes the second jump and makes
// the loading state **silent** for a screen reader — a regression dressed as an improvement, and
// one nothing would catch: bars render, the build passes, and the screen looks better. So what is
// pinned here is the half that is easy to lose. The shape is the visible half and is not asserted
// pixel by pixel; the announcement is the half that can disappear without anyone noticing.

import { cleanup, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { Skeleton } from "../src/components/ui";

afterEach(cleanup);

describe("Skeleton", () => {
  it("still says what the sentence it replaced said", () => {
    render(() => <Skeleton label="Loading…" />);
    const region = screen.getByRole("status");
    expect(region.getAttribute("aria-label")).toBe("Loading…");
  });

  it("hides its bars from assistive tech, so the words are announced once and not the shape", () => {
    render(() => <Skeleton label="Loading…" rows={4} />);
    const region = screen.getByRole("status");
    const bars = region.querySelector("[aria-hidden='true']");
    expect(bars).not.toBeNull();
    expect(bars?.children).toHaveLength(4);
  });

  it("draws the number of rows the caller asked for, and never zero", () => {
    render(() => <Skeleton label="a" rows={1} />);
    expect(screen.getByRole("status").querySelector("[aria-hidden='true']")?.children).toHaveLength(
      1,
    );
    cleanup();
    // A caller computing rows from a length can reach 0 or a negative; a placeholder that draws
    // nothing is an invisible loading state, which is the failure this clamp exists to prevent.
    render(() => <Skeleton label="a" rows={0} />);
    expect(screen.getByRole("status").querySelector("[aria-hidden='true']")?.children).toHaveLength(
      1,
    );
  });

  it("ends on a short bar, so it reads as content arriving rather than a widget", () => {
    render(() => <Skeleton label="a" rows={3} />);
    const bars = [...(screen.getByRole("status").querySelector("[aria-hidden='true']")?.children ?? [])];
    expect(bars.slice(0, -1).every((bar) => bar.className.includes("w-full"))).toBe(true);
    expect(bars.at(-1)?.className).toContain("w-2/3");
  });

  it("does not shorten the only bar there is", () => {
    render(() => <Skeleton label="a" rows={1} />);
    const only = screen.getByRole("status").querySelector("[aria-hidden='true']")?.children[0];
    expect(only?.className).toContain("w-full");
  });
});
