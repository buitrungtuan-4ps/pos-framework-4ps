// The cell panel (ADR-0179 C1): it names the cell, says when there is only one, and warns, without
// refusing anything, when the servers run outside the home country.

import { cleanup, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CellPanel } from "../src/components/CellPanel";

const getCell = vi.fn();

vi.mock("../src/api/cell", () => ({ getCell: () => getCell() }));

const CONFIGURED = {
  cell_id: "vn1",
  country_alias: "vn",
  home_country: "VN",
  base_url: "https://vn1.example.com",
  runs_in: "VN",
  runs_abroad: false,
  configured: true,
};

afterEach(() => {
  cleanup();
  getCell.mockReset();
});

describe("CellPanel", () => {
  it("shows every field of a configured cell", async () => {
    getCell.mockResolvedValue(CONFIGURED);
    render(() => <CellPanel />);
    expect(await screen.findByText("https://vn1.example.com")).toBeTruthy();
    expect(screen.getAllByText("vn1").length).toBeGreaterThan(0);
    expect(screen.getAllByText("VN").length).toBe(2);
    expect(screen.queryByText(/its servers run in/)).toBeNull();
  });

  it("says a cloud with no [cell] section is the single cell", async () => {
    getCell.mockResolvedValue({
      cell_id: "default",
      country_alias: null,
      home_country: null,
      base_url: null,
      runs_in: null,
      runs_abroad: false,
      configured: false,
    });
    render(() => <CellPanel />);
    expect(await screen.findByText("Single cell")).toBeTruthy();
    expect(screen.getByText(/has no \[cell\] section/)).toBeTruthy();
    expect(screen.getAllByText("Not set").length).toBe(4);
  });

  it("warns when the servers run abroad", async () => {
    getCell.mockResolvedValue({ ...CONFIGURED, runs_in: "SG", runs_abroad: true });
    render(() => <CellPanel />);
    const warning = await screen.findByText(/homed in VN, but its servers run in SG/);
    expect(warning.getAttribute("role")).toBe("status");
  });
});
