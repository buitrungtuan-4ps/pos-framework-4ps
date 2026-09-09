// The console knew who was signed in and never said (Wave 3 · Stage 6).
//
// `GET /admin/whoami` has been fetched on mount since Track G1, and its answer was used for exactly
// one thing: hiding the nav entries a role cannot reach. So on a console where four roles see four
// different navs, an operator who could not find a screen had no way to tell whether that was the
// role or the screen, and an operator with two accounts had nothing to say which one this tab was.
//
// The theme is the same absence from the other side — `tokens.css` has honoured `data-theme` since
// P6 and nothing set it. Both now live in one account menu, together with the locale switch and
// sign-out that were standing loose in the header.
//
// Identity here is a placeholder, not a person: `AdminIdentity` is T1 under the data-handling rules,
// so the fixture is an obvious stand-in rather than anything resembling a real admin.

import { MemoryRouter, Route } from "@solidjs/router";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { AccountMenu } from "../src/components/AccountMenu";
import { setActingAdmin } from "../src/state/session";

const FIXTURE = {
  id: "01M22190WCY5PS7KCA7ET7H679",
  email: "placeholder@example.test",
  name: "Placeholder Person",
  role: "ops",
  status: "active",
} as const;

vi.mock("../src/api/client", () => ({
  api: {},
  ApiError: class ApiError extends Error {},
}));

function mount(onSignOut: () => void = () => undefined) {
  render(() => (
    <MemoryRouter>
      <Route path="*" component={() => <AccountMenu onSignOut={onSignOut} />} />
    </MemoryRouter>
  ));
}

async function openMenu() {
  fireEvent.click(screen.getByRole("button", { name: "Your account" }));
  await waitFor(() => expect(screen.getByRole("radiogroup")).toBeTruthy());
}

beforeEach(() => {
  localStorage.clear();
  delete document.documentElement.dataset["theme"];
  setActingAdmin(null);
});

afterEach(cleanup);

describe("the account menu", () => {
  it("names the signed-in admin, their address and their role", async () => {
    setActingAdmin({ ...FIXTURE });
    mount();
    await openMenu();
    expect(screen.getByText(FIXTURE.name)).toBeTruthy();
    // The email is what the server knows this session by, so it is the field an operator with two
    // accounts actually checks.
    expect(screen.getByText(FIXTURE.email)).toBeTruthy();
    // The role explains the nav: four roles see four different sets of entries.
    expect(screen.getByText("Operations")).toBeTruthy();
  });

  it("opens and works with no identity, which is when signing out matters most", async () => {
    // `actingAdmin` is null both before whoami answers and after it fails, and the menu cannot tell
    // those apart. What it must not do is become unusable in either.
    mount();
    await openMenu();
    expect(screen.queryByText("Signed in as")).toBeNull();
    expect(screen.getByRole("radio", { name: "Dark" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Sign out" })).toBeTruthy();
  });

  it("closes on Escape, which is the only way out for a keyboard", async () => {
    // `useEscape` was private to `kit.tsx`, so the modal and the drawer had this and the three
    // header dropdowns did not — meaning the only way to dismiss one was to tab back through
    // whatever it contained and press the trigger again. Extracting the helper to `lib/escape.ts`
    // gave it to all three; this is the one with a test because it is the one being added.
    mount();
    await openMenu();
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("radiogroup")).toBeNull());
  });

  it("calls back on sign-out rather than logging out itself", async () => {
    // The menu does not own the session. `Shell` does — it clears the auth flag and navigates — so
    // the menu raising an event is what keeps one place responsible for ending a session.
    const onSignOut = vi.fn();
    mount(onSignOut);
    await openMenu();
    fireEvent.click(screen.getByRole("button", { name: "Sign out" }));
    expect(onSignOut).toHaveBeenCalledTimes(1);
  });
});

describe("the theme switcher", () => {
  it("offers three choices with system checked on a first run", async () => {
    mount();
    await openMenu();
    const radios = screen.getAllByRole("radio");
    expect(radios.map((radio) => radio.textContent)).toEqual(["System", "Light", "Dark"]);
    expect(screen.getByRole("radio", { name: "System" }).getAttribute("aria-checked")).toBe("true");
  });

  it("applies a choice to the root element and remembers it", async () => {
    mount();
    await openMenu();
    fireEvent.click(screen.getByRole("radio", { name: "Dark" }));
    await waitFor(() =>
      expect(screen.getByRole("radio", { name: "Dark" }).getAttribute("aria-checked")).toBe("true"),
    );
    expect(document.documentElement.dataset["theme"]).toBe("dark");
    expect(localStorage.getItem("pos.dashboard.theme")).toBe("dark");
  });

  it("clears the attribute when the operator goes back to system", async () => {
    // The case worth a test. `data-theme="system"` would still match the stylesheet's
    // `:not([data-theme="light"])`, so an operator on a light machine asking for "system" would be
    // handed dark — the attribute has to go, not change.
    mount();
    await openMenu();
    fireEvent.click(screen.getByRole("radio", { name: "Light" }));
    await waitFor(() => expect(document.documentElement.dataset["theme"]).toBe("light"));
    fireEvent.click(screen.getByRole("radio", { name: "System" }));
    await waitFor(() => expect(document.documentElement.hasAttribute("data-theme")).toBe(false));
    expect(localStorage.getItem("pos.dashboard.theme")).toBe("system");
  });
});

describe("the folded-in controls", () => {
  it("carry the locale switch, so removing it from the header lost nothing", async () => {
    mount();
    await openMenu();
    const select = screen.getByRole("combobox", { name: "Language" });
    expect([...(select as HTMLSelectElement).options].map((option) => option.textContent)).toEqual([
      "English",
      "Tiếng Việt",
    ]);
  });

  it("reach the two account screens by name", async () => {
    mount();
    await openMenu();
    expect(screen.getByRole("link", { name: "My sessions" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "My security" })).toBeTruthy();
  });
});
