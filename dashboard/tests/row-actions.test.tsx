// Tests for RowActions popover menu dismissal and keyboard focus restoration.

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { RowActions } from "../src/components/kit";

afterEach(cleanup);

describe("RowActions component", () => {
  it("opens and closes the action menu on trigger click", () => {
    render(() => (
      <RowActions label="Actions">
        <button type="button">Edit</button>
      </RowActions>
    ));

    const trigger = screen.getByRole("button", { name: "Actions" });
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByRole("menu")).toBeNull();

    fireEvent.click(trigger);
    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByRole("menu")).not.toBeNull();
    expect(screen.getByRole("button", { name: "Edit" })).not.toBeNull();

    fireEvent.click(trigger);
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("closes the menu on outside click via pointerdown", () => {
    render(() => (
      <div>
        <div data-testid="outside">Outside area</div>
        <RowActions label="Actions">
          <button type="button">Delete</button>
        </RowActions>
      </div>
    ));

    const trigger = screen.getByRole("button", { name: "Actions" });
    fireEvent.click(trigger);
    expect(screen.getByRole("menu")).not.toBeNull();

    const outside = screen.getByTestId("outside");
    fireEvent.pointerDown(outside);

    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("restores focus to trigger button when dismissed with Escape key", () => {
    render(() => (
      <RowActions label="Actions">
        <button type="button">Action item</button>
      </RowActions>
    ));

    const trigger = screen.getByRole("button", { name: "Actions" });
    trigger.focus();
    fireEvent.click(trigger);

    const actionItem = screen.getByRole("button", { name: "Action item" });
    actionItem.focus();
    expect(document.activeElement).toBe(actionItem);

    fireEvent.keyDown(window, { key: "Escape" });

    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });
});
