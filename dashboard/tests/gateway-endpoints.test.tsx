// The gateway endpoints screen (ADR-0198): it lists a tenant's endpoints with only the names of
// their secret headers, creates one from the typed fields and headers, edits one at the version it
// was read at keeping or removing a saved header, deletes one, and says which line it cannot read.

import { MemoryRouter, Route } from "@solidjs/router";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { fieldsFromText, fieldsToText, headersFromText } from "../src/lib/gateway-fields";
import { GatewayEndpoints } from "../src/screens/GatewayEndpoints";
import { setStoreId, setTenantId } from "../src/state/session";

const TENANT = "01M22190WCY5PS7KCA7ET7H679";
const ID = "01M2219QK4T3W6Z0Y8FBQ2X5M1";

const listGatewayEndpoints = vi.fn();
const createGatewayEndpoint = vi.fn();
const updateGatewayEndpoint = vi.fn();
const deleteGatewayEndpoint = vi.fn();

vi.mock("../src/api/gateway", () => ({
  listGatewayEndpoints: (tenant: string) => listGatewayEndpoints(tenant),
  createGatewayEndpoint: (tenant: string, input: unknown) => createGatewayEndpoint(tenant, input),
  updateGatewayEndpoint: (tenant: string, id: string, etag: string, input: unknown) =>
    updateGatewayEndpoint(tenant, id, etag, input),
  deleteGatewayEndpoint: (tenant: string, id: string) => deleteGatewayEndpoint(tenant, id),
}));

const ROW = {
  gateway_endpoint_id: ID,
  display_name: "Demo loyalty",
  url: "https://93.184.216.34/balance",
  method: "GATEWAY_METHOD_POST",
  request_fields: [{ name: "card_code", kind: "GATEWAY_FIELD_KIND_STRING", required: true }],
  response_fields: [{ name: "points", kind: "GATEWAY_FIELD_KIND_INTEGER", required: false }],
  deadline_ms: 3000,
  idempotent: true,
  enabled: true,
  secret_headers_set: ["x-api-key"],
  etag: "7",
};

function mount() {
  render(() => (
    <MemoryRouter>
      <Route path="/" component={GatewayEndpoints} />
    </MemoryRouter>
  ));
}

beforeEach(() => {
  localStorage.clear();
  setTenantId(TENANT);
  setStoreId("");
  listGatewayEndpoints.mockResolvedValue([ROW]);
});

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

describe("the gateway endpoint form's text", () => {
  it("reads fields one per line, and names the line it cannot read", () => {
    expect(fieldsFromText("card_code text required\n\n visits number ")).toEqual({
      fields: [
        { name: "card_code", kind: "GATEWAY_FIELD_KIND_STRING", required: true },
        { name: "visits", kind: "GATEWAY_FIELD_KIND_INTEGER", required: false },
      ],
    });
    expect(fieldsFromText("ok yes-no\nbad money")).toEqual({ badLine: 2 });
    expect(fieldsFromText("a text maybe")).toEqual({ badLine: 1 });
    expect(fieldsToText(ROW.request_fields as never)).toBe("card_code text required");
  });

  it("reads secret headers as Name: value", () => {
    expect(headersFromText("X-Api-Key: abc:def\n")).toEqual({ headers: { "X-Api-Key": "abc:def" } });
    expect(headersFromText("no colon here")).toEqual({ badLine: 1 });
    expect(headersFromText("Empty:")).toEqual({ badLine: 1 });
  });
});

describe("the gateway endpoints screen", () => {
  it("lists an endpoint with its secret headers named, never their values", async () => {
    mount();
    expect(await screen.findByText("Demo loyalty")).toBeTruthy();
    expect(screen.getByText("POST https://93.184.216.34/balance")).toBeTruthy();
    expect(screen.getByText("x-api-key")).toBeTruthy();
  });

  it("creates an endpoint from the typed fields and headers", async () => {
    listGatewayEndpoints.mockResolvedValue([]);
    createGatewayEndpoint.mockResolvedValue(ROW);
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "New endpoint" }));
    fireEvent.input(await screen.findByLabelText(/^Name/), { target: { value: "Demo loyalty" } });
    fireEvent.input(screen.getByLabelText(/^Address/), { target: { value: "https://93.184.216.34/balance" } });
    fireEvent.input(screen.getByLabelText(/^What a lookup sends/), { target: { value: "card_code text required" } });
    fireEvent.input(screen.getByLabelText(/^Secret headers/), { target: { value: "X-Api-Key: demo-key" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(createGatewayEndpoint).toHaveBeenCalled());
    expect(createGatewayEndpoint.mock.calls[0]?.[1]).toMatchObject({
      display_name: "Demo loyalty",
      method: "GATEWAY_METHOD_GET",
      deadline_ms: 3000,
      request_fields: [{ name: "card_code", kind: "GATEWAY_FIELD_KIND_STRING", required: true }],
      secret_headers: { "X-Api-Key": "demo-key" },
      clear_secret_headers: [],
    });
  });

  it("refuses a line it cannot read before anything is sent", async () => {
    listGatewayEndpoints.mockResolvedValue([]);
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "New endpoint" }));
    fireEvent.input(await screen.findByLabelText(/^Name/), { target: { value: "Demo" } });
    fireEvent.input(screen.getByLabelText(/^What the answer holds/), { target: { value: "points money" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText(/What the answer holds, line 1/)).toBeTruthy();
    expect(createGatewayEndpoint).not.toHaveBeenCalled();
  });

  it("edits at the version it read, keeping a header it does not type and removing one it ticks", async () => {
    updateGatewayEndpoint.mockResolvedValue(ROW);
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "Actions" }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit" }));
    fireEvent.click(await screen.findByLabelText(/Remove the saved x-api-key header/));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(updateGatewayEndpoint).toHaveBeenCalled());
    const [tenant, id, etag, input] = updateGatewayEndpoint.mock.calls[0] ?? [];
    expect([tenant, id, etag]).toEqual([TENANT, ID, "7"]);
    expect(input).toMatchObject({ secret_headers: {}, clear_secret_headers: ["x-api-key"], idempotent: true });
  });

  it("deletes an endpoint once its name is typed", async () => {
    deleteGatewayEndpoint.mockResolvedValue(undefined);
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "Actions" }));
    fireEvent.click(await screen.findByRole("button", { name: "Delete" }));
    fireEvent.input(await screen.findByLabelText(/Type the endpoint's name to confirm/), {
      target: { value: "Demo loyalty" },
    });
    const buttons = screen.getAllByRole("button", { name: "Delete" });
    fireEvent.click(buttons[buttons.length - 1]!);
    await waitFor(() => expect(deleteGatewayEndpoint).toHaveBeenCalledWith(TENANT, ID));
  });
});
