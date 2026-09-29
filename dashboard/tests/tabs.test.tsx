import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import { Tabs } from "../src/components/kit";

afterEach(cleanup);

describe("the Tabs component", () => {
  const tabs = [
    { key: "first", label: "First Tab" },
    { key: "second", label: "Second Tab" },
    { key: "third", label: "Third Tab" },
  ];

  it("renders tablist with correct WAI-ARIA roles, aria-selected and tabindex", () => {
    const onSelect = vi.fn();
    render(() => (
      <Tabs
        tabs={tabs}
        active="second"
        onSelect={onSelect}
        label="Category view"
      />
    ));

    const tablist = screen.getByRole("tablist", { name: "Category view" });
    expect(tablist).toBeTruthy();

    const tabButtons = screen.getAllByRole("tab");
    expect(tabButtons.length).toBe(3);

    // Active tab has aria-selected="true" and tabindex="0"
    expect(tabButtons[1]?.getAttribute("aria-selected")).toBe("true");
    expect(tabButtons[1]?.getAttribute("tabindex")).toBe("0");

    // Inactive tabs have aria-selected="false" and tabindex="-1"
    expect(tabButtons[0]?.getAttribute("aria-selected")).toBe("false");
    expect(tabButtons[0]?.getAttribute("tabindex")).toBe("-1");
    expect(tabButtons[2]?.getAttribute("aria-selected")).toBe("false");
    expect(tabButtons[2]?.getAttribute("tabindex")).toBe("-1");
  });

  it("calls onSelect when a tab is clicked", () => {
    const onSelect = vi.fn();
    render(() => (
      <Tabs
        tabs={tabs}
        active="first"
        onSelect={onSelect}
        label="Category view"
      />
    ));

    const tabButtons = screen.getAllByRole("tab");
    fireEvent.click(tabButtons[2]!);
    expect(onSelect).toHaveBeenCalledWith("third");
  });

  it("navigates tabs via ArrowLeft and ArrowRight keys", () => {
    const onSelect = vi.fn();
    render(() => (
      <Tabs
        tabs={tabs}
        active="second"
        onSelect={onSelect}
        label="Category view"
      />
    ));

    const tabButtons = screen.getAllByRole("tab");

    // Press ArrowRight from second tab -> selects third
    fireEvent.keyDown(tabButtons[1]!, { key: "ArrowRight" });
    expect(onSelect).toHaveBeenCalledWith("third");

    // Press ArrowLeft from second tab -> selects first
    fireEvent.keyDown(tabButtons[1]!, { key: "ArrowLeft" });
    expect(onSelect).toHaveBeenCalledWith("first");
  });

  it("navigates to first/last tab via Home and End keys", () => {
    const onSelect = vi.fn();
    render(() => (
      <Tabs
        tabs={tabs}
        active="second"
        onSelect={onSelect}
        label="Category view"
      />
    ));

    const tabButtons = screen.getAllByRole("tab");

    // Press Home key -> selects first
    fireEvent.keyDown(tabButtons[1]!, { key: "Home" });
    expect(onSelect).toHaveBeenCalledWith("first");

    // Press End key -> selects third
    fireEvent.keyDown(tabButtons[1]!, { key: "End" });
    expect(onSelect).toHaveBeenCalledWith("third");
  });
});
