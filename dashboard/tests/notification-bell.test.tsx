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

  it("closes when clicking outside the bell container", async () => {
    render(() => <NotificationBell />);
    const button = screen.getByRole("button", { name: "Notifications" });

    fireEvent.click(button);
    await waitFor(() => {
      expect(button.getAttribute("aria-expanded")).toBe("true");
    });

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
});
