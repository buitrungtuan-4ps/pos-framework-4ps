import { cleanup, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { Button } from "../src/components/ui";

afterEach(cleanup);

describe("the Button component", () => {
  it("renders with focus-visible accessibility styling", () => {
    render(() => <Button>Click me</Button>);
    const button = screen.getByRole("button", { name: "Click me" });
    expect(button).toBeTruthy();
    expect(button.className).toContain("focus-visible:outline-2");
    expect(button.className).toContain("focus-visible:outline-offset-2");
    expect(button.className).toContain("focus-visible:outline-accent");
  });
});
