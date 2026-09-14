import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { NotificationBell } from "../src/components/Toast";

afterEach(cleanup);

describe("NotificationBell component accessibility & interaction", () => {
  it("renders button with correct ARIA accessibility attributes", () => {
    render(() => <NotificationBell />);
    const button = screen.getByRole("button", { name: "Notifications" });
    expect(button.getAttribute("aria-haspopup")).toBe("true");
    expect(button.getAttribute("aria-expanded")).toBe("false");
  });

  it("expands history list and updates aria-expanded attribute when clicked", async () => {
    render(() => <NotificationBell />);
    const button = screen.getByRole("button", { name: "Notifications" });

    fireEvent.click(button);
    await waitFor(() => {
      expect(button.getAttribute("aria-expanded")).toBe("true");
    });

    const region = screen.getByRole("region", { name: "Notifications" });
    expect(region).toBeTruthy();
  });
});
