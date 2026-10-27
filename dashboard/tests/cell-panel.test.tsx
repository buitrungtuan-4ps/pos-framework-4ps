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
  directory_mode: "CELL_DIRECTORY_SIGNED",
  directory_version: 7,
  directory_refusal: null,
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
    expect(screen.getByText("Signed, version 7")).toBeTruthy();
  });

  it("says why the last directory was refused, beside the one still held", async () => {
    getCell.mockResolvedValue({
      ...CONFIGURED,
      directory_refusal: "the directory's signature does not verify",
    });
    render(() => <CellPanel />);
    const refusal = await screen.findByText(/signature does not verify/);
    expect(refusal.textContent).toContain("the one held is kept");
    expect(screen.getByText("Signed, version 7")).toBeTruthy();
  });

  it("says a cell with a key and no accepted directory answers for no tenant", async () => {
    getCell.mockResolvedValue({
      ...CONFIGURED,
      directory_mode: "CELL_DIRECTORY_MISSING",
      directory_version: null,
    });
    render(() => <CellPanel />);
    expect(await screen.findByText(/answers for no tenant/)).toBeTruthy();
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
      directory_mode: "CELL_DIRECTORY_SELF",
      directory_version: null,
      directory_refusal: null,
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
