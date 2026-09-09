// Finishing the new-store wizard has to hand over two links, not two screen names (Wave 3 · Stage 2).
//
// The wizard ends on "Store is ready" and closed with: *"Next: activate the store's devices
// (Activation) and publish its configuration (Configuration)."* Correct advice, addressed to
// somebody who already knows where those two screens are and can pick the new store in the top bar
// again — which is nobody who needed a wizard. Both are also the two steps without which the store
// cannot trade, so the sentence names the whole remaining critical path and helps with none of it.
//
// A missing link is the kind of defect nothing catches: prose naming a screen type-checks, renders,
// reads well, and passes every gate. So it is asserted here — both links present, and both carrying
// the store the wizard just created rather than whatever store happened to be in context before.

import { MemoryRouter, Route } from "@solidjs/router";
import { cleanup, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { NextSteps } from "../src/screens/NewStore";

const TENANT = "01M22190WCY5PS7KCA7ET7H679";
const STORE = "01M2219QK4T3W6Z0Y8FBQ2X5MV";

/** The store in context *before* the wizard ran — the one the links must not point at. */
const OTHER_STORE = "01M221A7XN6R4H9V2C0KDPJ8ZE";

function mountNextSteps(store = STORE) {
  render(() => (
    <MemoryRouter>
      <Route path="/" component={() => <NextSteps tenant={TENANT} store={store} />} />
    </MemoryRouter>
  ));
}

afterEach(cleanup);

describe("the wizard's closing step", () => {
  it("links to activation, scoped to the store it just created", () => {
    mountNextSteps();
    const link = screen.getByRole("link", { name: "Activate this store's devices" });
    expect(link.getAttribute("href")).toBe(`/t/${TENANT}/activation?store=${STORE}`);
  });

  it("links to configuration, scoped to the store it just created", () => {
    mountNextSteps();
    const link = screen.getByRole("link", { name: "Publish this store's configuration" });
    expect(link.getAttribute("href")).toBe(`/t/${TENANT}/config?store=${STORE}`);
  });

  it("carries the store it is given, so an unrelated store in context cannot leak in", () => {
    mountNextSteps(OTHER_STORE);
    for (const name of ["Activate this store's devices", "Publish this store's configuration"]) {
      expect(screen.getByRole("link", { name }).getAttribute("href")).toContain(OTHER_STORE);
    }
  });

  it("offers both steps and nothing else, so neither is the one an operator misses", () => {
    mountNextSteps();
    expect(screen.getAllByRole("link")).toHaveLength(2);
  });
});
