// The HQ panel (ADR-0179 C10): per store in another cell, its counts or why it was not read, and a
// plain line when this cell reads no other. Only counts are shown, never a key.

import { cleanup, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import { HqCellsPanel, totalEvents } from "../src/components/HqCellsPanel";

const getHqRollups = vi.fn();

vi.mock("../src/api/hq", () => ({ getHqRollups: () => getHqRollups() }));

const READ = {
  cell_id: "jp1",
  store_id: "01M2219QK4T3W6Z0Y8FBQ2X5MV",
  rollups: [
    { business_date: "2026-10-26", total_events: 41, by_type: { "sales.order.created": 41 } },
    { business_date: "2026-10-27", total_events: 12, by_type: { "sales.order.created": 12 } },
  ],
  error: null,
};

const REFUSED = {
  cell_id: "jp1",
  store_id: "01M22190WCY5PS7KCA7ET7H679",
  rollups: null,
  error: "https://jp1.example.com does not accept this key: ask that cell's operator for a new one",
};

afterEach(() => {
  cleanup();
  getHqRollups.mockReset();
});

describe("HqCellsPanel", () => {
  it("shows each store's days and events, and says only counts cross", async () => {
    getHqRollups.mockResolvedValue({ configured: true, stores: [READ] });
    render(() => <HqCellsPanel />);
    expect(await screen.findByText("2 days read, 53 events")).toBeTruthy();
    expect(screen.getByText(`Store ${READ.store_id} in cell jp1`)).toBeTruthy();
    expect(screen.getByText(/no personal data/)).toBeTruthy();
    expect(screen.queryByText(/not read$/)).toBeNull();
  });

  it("says why a store was not read and counts the stores not read", async () => {
    getHqRollups.mockResolvedValue({ configured: true, stores: [READ, REFUSED] });
    render(() => <HqCellsPanel />);
    const why = await screen.findByText(/ask that cell's operator/);
    expect(why.getAttribute("role")).toBe("status");
    expect(why.textContent).toMatch(/^Not read: /);
    expect(screen.getByText("1 not read")).toBeTruthy();
    expect(screen.getByText("2 days read, 53 events")).toBeTruthy();
  });

  it("says a cell with no [federation] section reads no other cell", async () => {
    getHqRollups.mockResolvedValue({ configured: false, stores: [] });
    render(() => <HqCellsPanel />);
    expect(await screen.findByText(/has no \[federation\] section/)).toBeTruthy();
    expect(screen.queryByText(/no personal data/)).toBeNull();
  });

  it("totals a store's events, and a store not read has none", () => {
    expect(totalEvents(READ)).toBe(53);
    expect(totalEvents(REFUSED)).toBe(0);
  });
});
