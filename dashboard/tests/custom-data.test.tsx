// The Custom data screen, written by `cargo xtask new screen --app dashboard`: it draws its title.
// Test what the screen does here as it grows.

import { cleanup, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { t } from "../src/i18n";
import { CustomData } from "../src/screens/CustomData";

afterEach(cleanup);

describe("CustomData", () => {
  it("draws its title", () => {
    render(() => <CustomData />);
    expect(screen.getByRole("heading", { name: t("nav.customData") })).toBeTruthy();
  });
});
