import { MemoryRouter, Route } from "@solidjs/router";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { CommandPalette, openPalette } from "../src/components/CommandPalette";

vi.mock("../src/api/client", () => ({
  api: {},
  ApiError: class ApiError extends Error {},
}));

function mount() {
  render(() => (
    <MemoryRouter>
      <Route path="*" component={() => <CommandPalette />} />
    </MemoryRouter>
  ));
}

beforeEach(() => {
  localStorage.clear();
});

afterEach(cleanup);

describe("the command palette", () => {
  it("opens on openPalette call and renders accessible dialog and combobox", async () => {
    mount();
    openPalette();
    await waitFor(() => expect(screen.getByRole("dialog")).toBeTruthy());

    const dialog = screen.getByRole("dialog");
    expect(dialog.getAttribute("aria-modal")).toBe("true");

    const input = screen.getByRole("combobox");
    expect(input.getAttribute("aria-expanded")).toBe("true");
    expect(input.getAttribute("aria-controls")).toBe("command-palette-results");

    const listbox = screen.getByRole("listbox");
    expect(listbox.id).toBe("command-palette-results");

    const options = screen.getAllByRole("option");
    expect(options.length).toBeGreaterThan(0);
    const firstOption = options[0];
    expect(firstOption).toBeDefined();
    if (firstOption) {
      expect(firstOption.getAttribute("aria-selected")).toBe("true");
    }
  });

  it("navigates selection with arrow keys and updates aria-selected", async () => {
    mount();
    openPalette();
    await waitFor(() => expect(screen.getByRole("dialog")).toBeTruthy());

    const input = screen.getByRole("combobox");
    const options = screen.getAllByRole("option");

    const opt0 = options[0];
    const opt1 = options[1];
    expect(opt0).toBeDefined();
    expect(opt1).toBeDefined();

    if (opt0 && opt1) {
      expect(opt0.getAttribute("aria-selected")).toBe("true");

      fireEvent.keyDown(input, { key: "ArrowDown" });
      await waitFor(() => expect(opt1.getAttribute("aria-selected")).toBe("true"));

      fireEvent.keyDown(input, { key: "ArrowUp" });
      await waitFor(() => expect(opt0.getAttribute("aria-selected")).toBe("true"));
    }
  });
});
