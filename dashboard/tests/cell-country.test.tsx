// A store whose country differs from its cell's (ADR-0179 C6): the hub card warns and takes an
// answer, and the store settings hint warns before anything is published. Neither refuses anything.

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { CellCountryCard } from "../src/components/CellCountryCard";
import { setStoreId, setTenantId } from "../src/state/session";

const TENANT = "01M22190WCY5PS7KCA7ET7H679";
const STORE = "01M2219QK4T3W6Z0Y8FBQ2X5MV";

const getCell = vi.fn();
const getCellStanding = vi.fn();
const acknowledgeCell = vi.fn();

vi.mock("../src/api/cell", () => ({
  getCell: () => getCell(),
  getCellStanding: (tenant: string, store: string) => getCellStanding(tenant, store),
  acknowledgeCell: (tenant: string, store: string, body: unknown) =>
    acknowledgeCell(tenant, store, body),
}));

const DIFFERS = {
  cell_id: "vn1",
  cell_country: "VN",
  profile_country: "JP",
  differs: true,
  acknowledgement: null,
};

beforeEach(() => {
  localStorage.clear();
  setTenantId(TENANT);
  setStoreId(STORE);
});

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

describe("CellCountryCard", () => {
  it("draws nothing for a store that agrees with its cell", async () => {
    getCellStanding.mockResolvedValue({ ...DIFFERS, profile_country: "VN", differs: false });
    render(() => <CellCountryCard />);
    await waitFor(() => expect(getCellStanding).toHaveBeenCalledWith(TENANT, STORE));
    expect(screen.queryByText(/whose home country is/)).toBeNull();
  });

  it("warns about a difference and records an answer, then re-reads", async () => {
    getCellStanding.mockResolvedValueOnce(DIFFERS).mockResolvedValueOnce({
      ...DIFFERS,
      acknowledgement: {
        profile_country: "JP",
        cell_country: "VN",
        reason: "Group DTA.",
        acknowledged_at_ms: Date.now(),
      },
    });
    acknowledgeCell.mockResolvedValue({});
    render(() => <CellCountryCard />);
    expect(await screen.findByText(/registered in JP.*cell vn1.*VN/)).toBeTruthy();
    fireEvent.input(screen.getByRole("textbox"), { target: { value: "Group DTA." } });
    fireEvent.click(screen.getByRole("button"));
    await waitFor(() =>
      expect(acknowledgeCell).toHaveBeenCalledWith(TENANT, STORE, {
        profile_country: "JP",
        cell_country: "VN",
        reason: "Group DTA.",
      }),
    );
    expect(await screen.findByText("Group DTA.")).toBeTruthy();
    expect(getCellStanding).toHaveBeenCalledTimes(2);
  });

  it("shows a refusal beside the form", async () => {
    getCellStanding.mockResolvedValue(DIFFERS);
    acknowledgeCell.mockRejectedValue(new Error("the store has moved"));
    render(() => <CellCountryCard />);
    await screen.findByText(/registered in JP/);
    fireEvent.input(screen.getByRole("textbox"), { target: { value: "Because." } });
    fireEvent.click(screen.getByRole("button"));
    expect(await screen.findByText(/the store has moved/)).toBeTruthy();
  });
});

describe("countryHint", () => {
  it("adds the cell warning only for a country that is not the cell's", async () => {
    getCell.mockResolvedValue({ home_country: "VN" });
    const { cellHomeCountry, countryHint } = await import("../src/lib/cell-country");
    cellHomeCountry();
    await waitFor(() => expect(cellHomeCountry()).toBe("VN"));
    expect(countryHint("vn")).not.toMatch(/not this cell's home country/);
    expect(countryHint("")).not.toMatch(/not this cell's home country/);
    expect(countryHint("jp")).toMatch(/JP is not this cell's home country \(VN\)/);
  });
});
