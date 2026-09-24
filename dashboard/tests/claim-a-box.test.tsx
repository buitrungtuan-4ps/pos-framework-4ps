// Claiming a box that shows a code (ADR-0148).
//
// A box installed from the one image for every store shows an eight-character code; the operator
// types it on the Activation screen and picks the device it becomes. Two things are pinned: the code
// goes to the cloud as the operator typed it, bound to the chosen device of the store in context,
// and an empty code is refused here rather than sent.

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Activation } from "../src/screens/Activation";
import { selectStore, selectTenant } from "../src/state/session";

const TENANT = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const STORE = { id: "01STOREAAAAAAAAAAAAAAAAAAA", name: "Thao Dien" };
const TILL = {
  device_id: "01DEVICEAAAAAAAAAAAAAAAAAA",
  tenant_id: TENANT.id,
  store_id: STORE.id,
  name: "Front till",
  kind: "pos",
  status: "active",
  etag: '"1"',
};

const bindClaim = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listDevices: () => Promise.resolve([TILL]),
    bindClaim: (...args: unknown[]) => bindClaim(...args),
    issueActivation: vi.fn(),
    createDevice: vi.fn(),
    updateDevice: vi.fn(),
  },
  ApiError: class ApiError extends Error {},
}));

async function openTheForm() {
  render(() => <Activation />);
  await waitFor(() => expect(screen.getByText("Front till")).toBeTruthy());
  fireEvent.click(screen.getByRole("button", { name: "Claim a box" }));
  await waitFor(() => expect(screen.getByLabelText(/^Code on the box/)).toBeTruthy());
}

describe("claim a box", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    bindClaim.mockResolvedValue(undefined);
    selectTenant(TENANT.id, TENANT.name);
    selectStore(STORE.id, STORE.name);
  });
  afterEach(cleanup);

  it("binds the typed code to the chosen device of this store", async () => {
    await openTheForm();
    fireEvent.input(screen.getByLabelText(/^Code on the box/), { target: { value: "ab10-cd0z" } });
    const submit = screen.getAllByRole("button", { name: "Claim a box" }).at(-1);
    fireEvent.click(submit as HTMLElement);
    await waitFor(() =>
      expect(bindClaim).toHaveBeenCalledWith(TENANT.id, STORE.id, TILL.device_id, "ab10-cd0z"),
    );
  });

  it("asks for the code rather than sending an empty one", async () => {
    await openTheForm();
    const submit = screen.getAllByRole("button", { name: "Claim a box" }).at(-1);
    fireEvent.click(submit as HTMLElement);
    await waitFor(() => expect(screen.getByText("Type the code the box shows.")).toBeTruthy());
    expect(bindClaim).not.toHaveBeenCalled();
  });
});
