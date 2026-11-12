// A webhook names the event types it is sent (roadmap-v3 B6.2): the table says which, or every
// event, and the registration form sends the tokens typed, split on commas and new lines.

import { MemoryRouter, Route } from "@solidjs/router";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { eventTypesTyped, Webhooks } from "../src/screens/Webhooks";
import { setStoreId, setTenantId } from "../src/state/session";

const TENANT = "01M22190WCY5PS7KCA7ET7H679";
const STORE = "01M22190WCY5PS7KCA7ET7H67A";

const listWebhooks = vi.fn();
const registerWebhook = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listWebhooks: (tenant: string) => listWebhooks(tenant),
    registerWebhook: (tenant: string, store: string, url: string, eventTypes: string[]) =>
      registerWebhook(tenant, store, url, eventTypes),
  },
  ApiError: class ApiError extends Error {
    details: unknown[] = [];
  },
}));

function row(id: string, url: string, eventTypes: string[]) {
  return { id, store_id: STORE, url, cursor: null, disabled: false, event_types: eventTypes };
}

beforeEach(() => {
  setTenantId(TENANT);
  setStoreId(STORE);
  listWebhooks.mockResolvedValue([
    row("01M2219QK4T3W6Z0Y8FBQ2X5M1", "https://a.example/all", []),
    row("01M2219QK4T3W6Z0Y8FBQ2X5M2", "https://b.example/some", ["billing.payment.captured", "sales.order.opened"]),
  ]);
  registerWebhook.mockResolvedValue({ id: "01M2219QK4T3W6Z0Y8FBQ2X5M3", url: "https://c.example/x", signing_secret: "s" });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("a webhook's event types", () => {
  it("are split on commas and white space, blanks dropped", () => {
    expect(eventTypesTyped(" sales.order.opened,\nbilling.payment.captured ,, ")).toEqual(["sales.order.opened", "billing.payment.captured"]);
    expect(eventTypesTyped("  ")).toEqual([]);
  });

  it("show in the table, every event when none is named, and are sent on registration", async () => {
    render(() => (
      <MemoryRouter>
        <Route path="/" component={Webhooks} />
      </MemoryRouter>
    ));
    expect(await screen.findByText("Every event")).toBeTruthy();
    expect(screen.getByText("billing.payment.captured, sales.order.opened")).toBeTruthy();

    fireEvent.click(screen.getAllByRole("button", { name: "Register an endpoint" })[0]!);
    fireEvent.input(screen.getByRole("textbox", { name: /Destination URL/ }), { target: { value: "https://c.example/x" } });
    fireEvent.input(screen.getByRole("textbox", { name: /Events sent/ }), { target: { value: "sales.order.opened, billing.bill.settled" } });
    fireEvent.submit(screen.getByRole("textbox", { name: /Events sent/ }).closest("form")!);
    await waitFor(() =>
      expect(registerWebhook).toHaveBeenCalledWith(TENANT, STORE, "https://c.example/x", ["sales.order.opened", "billing.bill.settled"]),
    );
  });
});
