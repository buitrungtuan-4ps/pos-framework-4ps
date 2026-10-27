// Where a vendor processes a connection's data (ADR-0179 C7): the table shows it and flags another
// country than the cell's, the editor takes it, and the hint warns before it is saved. Nothing is
// refused for a difference.

import { MemoryRouter, Route } from "@solidjs/router";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Integrations } from "../src/screens/Integrations";
import { setStoreId, setTenantId } from "../src/state/session";

const TENANT = "01M22190WCY5PS7KCA7ET7H679";

const listProviders = vi.fn();
const listConnections = vi.fn();
const createConnection = vi.fn();
const getCell = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listProviders: () => listProviders(),
    listConnections: (tenant: string) => listConnections(tenant),
    createConnection: (tenant: string, input: unknown) => createConnection(tenant, input),
  },
  ApiError: class ApiError extends Error {
    details: unknown[] = [];
  },
}));
vi.mock("../src/api/cell", () => ({ getCell: () => getCell() }));

const PROVIDER = {
  provider_id: "einvoice.test",
  family: "INTEGRATION_FAMILY_E_INVOICE",
  display_name_key: "einvoice.test",
  sandbox: false,
  fields: [],
};

function connection(processing: string | null, outside: boolean) {
  return {
    connection_id: "01M2219QK4T3W6Z0Y8FBQ2X5MV",
    provider_id: "einvoice.test",
    scope_level: "CONNECTION_SCOPE_TENANT",
    display_name: "E-invoices",
    enabled: true,
    settings: {},
    secrets_set: [],
    processing_country: processing,
    processing_outside_cell: outside,
    etag: '"1"',
  };
}

function mount() {
  render(() => (
    <MemoryRouter>
      <Route path="/" component={Integrations} />
    </MemoryRouter>
  ));
}

beforeEach(() => {
  localStorage.clear();
  setTenantId(TENANT);
  setStoreId("");
  listProviders.mockResolvedValue({ providers: [PROVIDER] });
  getCell.mockResolvedValue({ home_country: "VN" });
});

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

describe("a connection's processing country", () => {
  it("is flagged in the table when it is outside the cell's country", async () => {
    listConnections.mockResolvedValue([connection("SG", true)]);
    mount();
    expect(await screen.findByText("SG, outside this cell's country")).toBeTruthy();
  });

  it("reads as not recorded when it is unknown", async () => {
    listConnections.mockResolvedValue([connection(null, false)]);
    mount();
    expect(await screen.findByText("Not recorded")).toBeTruthy();
  });

  it("is sent upper-case, with the hint warning first", async () => {
    listConnections.mockResolvedValue([]);
    createConnection.mockResolvedValue(connection("SG", true));
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "Connect a provider" }));
    fireEvent.input(await screen.findByLabelText(/^Name/), { target: { value: "E-invoices" } });
    fireEvent.input(screen.getByLabelText(/^Processes data in/), { target: { value: "sg" } });
    expect(await screen.findByText(/SG is not this cell's home country \(VN\)/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(createConnection).toHaveBeenCalled());
    expect(createConnection.mock.calls[0]?.[1]).toMatchObject({ processing_country: "SG" });
  });
});
