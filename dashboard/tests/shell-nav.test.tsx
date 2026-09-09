// The nav's marker said the opposite of what it meant (Wave 3 · Stage 2).
//
// Every scoped nav entry drew a dot, filled with `bg-accent` — the brand colour, which in this
// palette is red — when the screen's context *was* ready, and left hollow and all but invisible
// when the screen was blocked. So a nav full of red meant everything was fine, the entries an
// operator could not use looked like ordinary entries, and the one thing a screen reader announced
// twenty times was "Context ready". Every dashboard convention reads a coloured dot as something
// needing attention; this one read it as approval.
//
// Nothing could catch that: the class name is correct CSS, the label is a real translation key, and
// both render perfectly. It is a defect of meaning, so it is asserted here — the marker exists only
// on an entry that cannot be opened yet, and says what that entry needs.
//
// The structural changes are checked here too, because each is a way the nav can go wrong
// silently: a group that hides the screen you are on, a drawer that cannot be opened, an accordion
// that lets two groups be open at once, and — the one the operator actually complained about — an
// open entry drawn in the same ground as any entry under the pointer, so that nothing on the screen
// said where they were.

import { createMemoryHistory, MemoryRouter, Route } from "@solidjs/router";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Shell } from "../src/components/Shell";
import { setStoreId, setTenantId } from "../src/state/session";

const TENANT = "01M22190WCY5PS7KCA7ET7H679";
const STORE = "01M2219QK4T3W6Z0Y8FBQ2X5MV";

const whoami = vi.fn();
const listTenants = vi.fn();
const listStores = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    whoami: () => whoami(),
    logout: () => Promise.resolve(),
    listTenants: () => listTenants(),
    listStores: (tenant: string) => listStores(tenant),
  },
  ApiError: class ApiError extends Error {},
}));

function mountShell(path = "/") {
  // `MemoryRouter` matches on a history object, not on an `initialEntries` prop, so the starting
  // path has to be pushed into one before the router reads it. The argument used to be accepted and
  // then ignored, which meant every test in here ran on `/` whatever it asked for.
  const history = createMemoryHistory();
  history.set({ value: path });
  render(() => (
    <MemoryRouter history={history}>
      <Route path="*" component={() => <Shell>{null}</Shell>} />
    </MemoryRouter>
  ));
  return history;
}

beforeEach(() => {
  localStorage.clear();
  setTenantId("");
  setStoreId("");
  whoami.mockResolvedValue({ role: "owner", email: "[EMAIL_REDACTED]" });
  listTenants.mockResolvedValue([]);
  listStores.mockResolvedValue([]);
});

afterEach(cleanup);

describe("the readiness marker", () => {
  it("marks a screen that cannot be opened yet, and says what it needs", async () => {
    mountShell();
    await waitFor(() => expect(whoami).toHaveBeenCalled());
    // No tenant and no store: the tenant-scoped and store-scoped entries in the open group are all
    // blocked, and each carries the marker naming the piece it is waiting on.
    expect(screen.getAllByLabelText("Needs a store").length).toBeGreaterThan(0);
    expect(screen.getAllByLabelText("Needs a tenant").length).toBeGreaterThan(0);
  });

  it("marks nothing once the context is there", async () => {
    setTenantId(TENANT);
    setStoreId(STORE);
    mountShell();
    await waitFor(() => expect(whoami).toHaveBeenCalled());
    // The state that used to be painted brand-red on every scoped entry is now unremarkable and
    // carries nothing at all.
    expect(screen.queryAllByLabelText("Needs a store")).toHaveLength(0);
    expect(screen.queryAllByLabelText("Needs a tenant")).toHaveLength(0);
  });

  it("never announces that a screen is fine", async () => {
    setTenantId(TENANT);
    mountShell();
    await waitFor(() => expect(whoami).toHaveBeenCalled());
    // "Context ready" was the label a screen reader met on twenty entries. The key is gone.
    expect(screen.queryByLabelText("Context ready")).toBeNull();
  });
});

describe("the collapsible groups", () => {
  it("opens the group holding the screen and leaves the rest closed", async () => {
    mountShell();
    await waitFor(() => expect(whoami).toHaveBeenCalled());
    expect(screen.getByRole("button", { name: /^Overview/ }).getAttribute("aria-expanded")).toBe(
      "true",
    );
    expect(
      screen.getByRole("button", { name: /^Menu & pricing/ }).getAttribute("aria-expanded"),
    ).toBe("false");
  });

  it("remembers the group the operator opened", async () => {
    mountShell();
    await waitFor(() => expect(whoami).toHaveBeenCalled());
    fireEvent.click(screen.getByRole("button", { name: /^Menu & pricing/ }));
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: /^Menu & pricing/ }).getAttribute("aria-expanded"),
      ).toBe("true"),
    );
    // Written through, so the next page load opens it too.
    expect(localStorage.getItem("pos.dashboard.navGroups")).toBe(
      JSON.stringify({ opened: "nav.group.menu" }),
    );
  });

  it("keeps one group open at a time", async () => {
    mountShell();
    await waitFor(() => expect(whoami).toHaveBeenCalled());
    fireEvent.click(screen.getByRole("button", { name: /^Menu & pricing/ }));
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: /^Menu & pricing/ }).getAttribute("aria-expanded"),
      ).toBe("true"),
    );
    // Opening the second closed the first. This is the whole point of the accordion: the nav is a
    // fixed, short shape — the headings plus at most one group's entries — however many groups the
    // console grows.
    const expanded = screen
      .getAllByRole("button")
      .filter((button) => button.getAttribute("aria-expanded") === "true")
      .filter((button) => button.getAttribute("aria-controls")?.startsWith("nav-group-"));
    expect(expanded).toHaveLength(1);
  });

  it("closes on a second click, and says so", async () => {
    mountShell();
    await waitFor(() => expect(whoami).toHaveBeenCalled());
    const overview = () => screen.getByRole("button", { name: /^Overview/ });
    fireEvent.click(overview());
    await waitFor(() => expect(overview().getAttribute("aria-expanded")).toBe("false"));
    // "I closed the lot" is a real answer and has to survive standing on a screen inside it —
    // otherwise the containment rule springs the group back open and the click did nothing.
    expect(localStorage.getItem("pos.dashboard.navGroups")).toBe(
      JSON.stringify({ opened: null }),
    );
  });

  it("marks a closed group that holds the screen you are on", async () => {
    mountShell();
    await waitFor(() => expect(whoami).toHaveBeenCalled());
    // The store hub is the screen, and it lives in Overview, so while Overview is open nothing is
    // marked — the entry itself says it.
    expect(screen.queryByLabelText("Contains the open screen")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /^Menu & pricing/ }));
    // Overview is now closed and its entries are `hidden`, so the heading is the only thing that
    // can say where the operator is. Without this the accordion can lose them.
    await waitFor(() => expect(screen.getByLabelText("Contains the open screen")).toBeTruthy());
    expect(screen.getByRole("button", { name: /Contains the open screen/ })).toBeTruthy();
  });

  it("keeps a closed group's entries out of the accessibility tree", async () => {
    mountShell();
    await waitFor(() => expect(whoami).toHaveBeenCalled());
    // Menu & pricing is closed, so its entries are `hidden` — not merely invisible. A nav that hid
    // entries with CSS alone would still hand a screen reader thirty links.
    //
    // Matched loosely because a blocked entry's accessible name is its label *plus* the marker's —
    // "Menu Needs a tenant" — which is the marker earning its place: a screen reader user hears
    // both the destination and why it is not open yet, in one announcement.
    expect(screen.queryByRole("link", { name: /^Menu/ })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /^Menu & pricing/ }));
    await waitFor(() => expect(screen.getByRole("link", { name: /^Menu/ })).toBeTruthy());
  });
});

describe("the entry you are standing on", () => {
  it("is the only one marked current", async () => {
    mountShell();
    await waitFor(() => expect(whoami).toHaveBeenCalled());
    // The complaint that started this: the open entry wore `surface-raised`, which is also what
    // every hovered entry wore, so "where am I" had no answer on screen. It now wears the accent
    // tint — and, for a screen reader, `aria-current`, which is the half that must be exactly one.
    const current = screen
      .getAllByRole("link")
      .filter((link) => link.getAttribute("aria-current") === "page");
    expect(current).toHaveLength(1);
    const [here] = current;
    expect(here?.className).toContain("bg-selected");
    // And the tint is not the hover ground: an entry that is merely hovered must not look open.
    expect(here?.className).not.toContain("hover:bg-canvas");
  });

  it("does not also mark the entry its path sits under", async () => {
    // `/stores/new` is the wizard. The link's own `activeClass` matches on a path *prefix*, so
    // Stores used to light up as well and the nav claimed two open screens. The nav reads the
    // screen table instead, which matches exactly.
    setTenantId(TENANT);
    mountShell(`/t/${TENANT}/stores/new`);
    await waitFor(() => expect(whoami).toHaveBeenCalled());
    fireEvent.click(screen.getByRole("button", { name: /^Stores & devices/ }));
    const stores = await waitFor(() => screen.getByRole("link", { name: /^Stores/ }));
    expect(stores.className).not.toContain("bg-selected");
    // Nothing at all is marked here, which is the honest answer: the wizard has no nav entry.
    expect(
      screen.getAllByRole("link").filter((link) => link.getAttribute("aria-current") === "page"),
    ).toEqual([]);
  });
});

describe("the drawer under md", () => {
  it("starts closed and opens from the header", async () => {
    mountShell();
    await waitFor(() => expect(whoami).toHaveBeenCalled());
    const toggle = screen.getByRole("button", { name: "Navigation" });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(toggle.getAttribute("aria-controls")).toBe("console-nav");
    fireEvent.click(toggle);
    await waitFor(() => expect(toggle.getAttribute("aria-expanded")).toBe("true"));
  });

  it("closes on Escape", async () => {
    mountShell();
    await waitFor(() => expect(whoami).toHaveBeenCalled());
    const toggle = screen.getByRole("button", { name: "Navigation" });
    fireEvent.click(toggle);
    await waitFor(() => expect(toggle.getAttribute("aria-expanded")).toBe("true"));
    fireEvent.keyDown(toggle, { key: "Escape" });
    await waitFor(() => expect(toggle.getAttribute("aria-expanded")).toBe("false"));
  });
});
