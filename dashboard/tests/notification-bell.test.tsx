// The notification bell, on the properties its markup has to hold: the trigger announces itself as
// a popup button so a screen reader says what pressing it does, the panel it opens is a landmark
// rather than an anonymous box, and the clear control is dead while there is nothing to clear — a
// button that looks live and does nothing reads as a fault.
//
// Order matters in this file. The history lives in a module-level signal that outlives `cleanup`,
// so every assertion about an empty history runs before anything raises a toast.

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { NotificationBell, ToastHost, toast } from "../src/components/Toast";

afterEach(cleanup);

describe("the notification bell", () => {
  it("names itself a popup trigger, and starts collapsed", () => {
    render(() => <NotificationBell />);
    const button = screen.getByRole("button", { name: "Notifications" });
    expect(button.getAttribute("aria-haspopup")).toBe("true");
    expect(button.getAttribute("aria-expanded")).toBe("false");
  });

  it("opens a labelled region, and says so on the trigger", async () => {
    render(() => <NotificationBell />);
    const button = screen.getByRole("button", { name: "Notifications" });

    fireEvent.click(button);
    await waitFor(() => {
      expect(button.getAttribute("aria-expanded")).toBe("true");
    });

    expect(screen.getByRole("region", { name: "Notifications" })).toBeTruthy();
  });

  it("closes when a pointer goes down outside it", async () => {
    render(() => <NotificationBell />);
    const button = screen.getByRole("button", { name: "Notifications" });
    fireEvent.click(button);
    expect(screen.getByRole("region", { name: "Notifications" })).toBeTruthy();

    fireEvent.pointerDown(document.body);

    await waitFor(() => {
      expect(button.getAttribute("aria-expanded")).toBe("false");
      expect(screen.queryByRole("region", { name: "Notifications" })).toBeNull();
    });
  });

  it("disables the clear control while the history is empty", () => {
    render(() => <NotificationBell />);
    fireEvent.click(screen.getByRole("button", { name: "Notifications" }));

    const clear = screen.getByRole("button", { name: "Clear all" }) as HTMLButtonElement;
    expect(clear.disabled).toBe(true);
  });

  it("enables the clear control once something arrives, and empties the list on click", async () => {
    toast.ok("Test notification message");

    render(() => <NotificationBell />);
    fireEvent.click(screen.getByRole("button", { name: "Notifications" }));

    const clear = screen.getByRole("button", { name: "Clear all" }) as HTMLButtonElement;
    expect(clear.disabled).toBe(false);
    expect(screen.getByText("Test notification message")).toBeTruthy();

    fireEvent.click(clear);

    await waitFor(() => {
      expect(clear.disabled).toBe(true);
    });
    expect(screen.getByText("No notifications yet.")).toBeTruthy();
  });

  it("renders live toast with accessible dismiss button carrying focus visible styles", () => {
    toast.error("An error occurred");
    render(() => <ToastHost />);

    const dismissButtons = screen.getAllByRole("button", { name: "Dismiss" });
    expect(dismissButtons.length).toBeGreaterThan(0);
    const btn = dismissButtons[0];
    expect(btn).toBeDefined();
    if (btn) {
      expect(btn.className).toContain("focus-visible:outline-2");
      expect(btn.className).toContain("focus-visible:outline-accent");
    }
  });

  // Focus, not only the highlight: a highlight that moves while focus stays on the bell is invisible
  // to a screen reader, which reads whatever has focus. Each press is sent from where focus is, the
  // way a real keyboard sends it.
  it("moves focus through the history with ArrowDown and ArrowUp, wrapping at the ends", async () => {
    toast.ok("Nav notice A");
    toast.ok("Nav notice B");

    render(() => <NotificationBell />);
    const button = screen.getByRole("button", { name: "Notifications" });
    fireEvent.click(button);

    await waitFor(() => {
      expect(screen.getByText("Nav notice B")).toBeTruthy();
    });

    const items = screen.getAllByRole("listitem");
    expect(items.length).toBeGreaterThanOrEqual(2);
    const first = items[0];
    const second = items[1];
    const last = items[items.length - 1];
    if (!first || !second || !last) throw new Error("the history should list at least two items");

    const press = (key: string) =>
      fireEvent.keyDown(document.activeElement ?? button, { key });

    button.focus();
    press("ArrowDown");
    expect(document.activeElement).toBe(first);
    expect(first.classList.contains("bg-surface-raised")).toBe(true);

    press("ArrowDown");
    expect(document.activeElement).toBe(second);
    expect(second.classList.contains("bg-surface-raised")).toBe(true);
    expect(first.classList.contains("bg-surface-raised")).toBe(false);

    press("ArrowUp");
    expect(document.activeElement).toBe(first);

    // Up from the first wraps to the last, as down from the last wraps to the first.
    press("ArrowUp");
    expect(document.activeElement).toBe(last);

    // Escape from inside the history closes it and gives focus back to the bell, rather than
    // dropping it on the page body along with the item it was on.
    press("Escape");
    await waitFor(() => {
      expect(button.getAttribute("aria-expanded")).toBe("false");
    });
    expect(document.activeElement).toBe(button);
  });
});
