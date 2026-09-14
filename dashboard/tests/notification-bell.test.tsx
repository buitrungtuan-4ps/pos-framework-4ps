import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { NotificationBell, toast } from "../src/components/Toast";

describe("NotificationBell", () => {
  afterEach(cleanup);

  it("disables clear button when history is empty", async () => {
    render(() => <NotificationBell />);

    const bellButton = screen.getByRole("button", { name: "Notifications" });
    fireEvent.click(bellButton);

    const clearButton = screen.getByRole("button", { name: "Clear all" });
    expect(clearButton).toBeTruthy();
    expect((clearButton as HTMLButtonElement).disabled).toBe(true);
  });

  it("enables clear button when history contains items and clears history on click", async () => {
    toast.ok("Test notification message");

    render(() => <NotificationBell />);
    const bellButton = screen.getByRole("button", { name: "Notifications" });
    fireEvent.click(bellButton);

    const clearButton = screen.getByRole("button", { name: "Clear all" });
    expect((clearButton as HTMLButtonElement).disabled).toBe(false);

    expect(screen.getByText("Test notification message")).toBeTruthy();

    fireEvent.click(clearButton);

    await waitFor(() => {
      expect((clearButton as HTMLButtonElement).disabled).toBe(true);
    });
    expect(screen.getByText("No notifications yet.")).toBeTruthy();
  });
});
