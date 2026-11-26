// The console publishes a store's integrations (phase 10 open item O5): the card on the
// integrations screen sends `PUT /admin/config/integrations` for the store in context, says what the
// store now carries, and shows a refusal, such as a delivery connection without its choices.

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { PublishIntegrations } from "../src/screens/integrations/PublishIntegrations";
import { setStoreId, setTenantId } from "../src/state/session";

const TENANT = "01M22190WCY5PS7KCA7ET7H679";
const STORE = "01M2219QK4T3W6Z0Y8FBQ2X5ST";

const publishIntegrations = vi.fn();
const configNodes = vi.fn();

vi.mock("../src/api/integrations", () => ({
  publishIntegrations: (tenant: string, store: string) => publishIntegrations(tenant, store),
}));
vi.mock("../src/api/client", () => ({
  api: {
    configNodes: () => configNodes(),
  },
  ApiError: class ApiError extends Error {
    details: unknown[] = [];
  },
}));

beforeEach(() => {
  localStorage.clear();
  setTenantId(TENANT);
  configNodes.mockResolvedValue({ nodes: [] });
});

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

describe("publishing a store's integrations", () => {
  it("publishes to the store in context and says what it carries", async () => {
    setStoreId(STORE);
    publishIntegrations.mockResolvedValue({ config_version_id: "01M2219QK4T3W6Z0Y8FBQ2XV01", integrations: 2, vendors: 1 });
    render(() => <PublishIntegrations connections={[]} />);
    fireEvent.click(await screen.findByRole("button", { name: "Publish integrations" }));
    await waitFor(() => expect(publishIntegrations).toHaveBeenCalledWith(TENANT, STORE));
    expect(await screen.findAllByText("Published: 2 connections and 1 delivery apps.")).not.toHaveLength(0);
  });

  it("asks for a store first", async () => {
    setStoreId("");
    render(() => <PublishIntegrations connections={[]} />);
    expect(await screen.findByText("Choose a store first.")).toBeTruthy();
    expect(publishIntegrations).not.toHaveBeenCalled();
  });
});
