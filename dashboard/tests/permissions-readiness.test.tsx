// "Before you turn this on" (ADR-0158, Rollout): what each role at a store does not grant.
//
// The panel exists so an owner can see, before a store enforces each person's own permissions,
// which roles would leave somebody unable to work. Four properties are pinned, because each is a way
// it could look helpful and mislead:
//
//   * a role's gaps are named in words, everyday ones first, so "Take a payment" is not buried
//     under a list of voids and refunds a role is meant to lack;
//   * when no role misses an everyday permission, the panel says so plainly rather than leaving the
//     operator to infer it from an absence;
//   * people who hold no role at all are counted, because enforced they can do nothing;
//   * each role links to its editor, so the fix is one click from the gap.

import { cleanup, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ApiError } from "../src/api/client";
import type { PermissionsReadiness } from "../src/api/types";
import { PermissionsReadinessPanel } from "../src/components/PermissionsReadiness";
import { setLocale } from "../src/i18n";

const TENANT = "01M22190WCY5PS7KCA7ET7H679";
const STORE = "01STOREAAAAAAAAAAAAAAAAAAA";
const CASHIER = "01ROLEAAAAAAAAAAAAAAAAAAAA";

/** A Cashier missing one everyday, one audited and one high-risk permission; a Lead missing none. */
const READINESS: PermissionsReadiness = {
  store_id: STORE,
  enforced: false,
  roles: [
    {
      role_template_id: CASHIER,
      name: "Cashier",
      people: 3,
      missing: [
        { id: "sales.line.fire", group: "SALES", risk: "LOW" },
        { id: "billing.receipt.reprint", group: "BILLING", risk: "MEDIUM" },
        { id: "billing.bill.void", group: "BILLING", risk: "HIGH" },
      ],
    },
    { role_template_id: "01ROLEBBBBBBBBBBBBBBBBBBBB", name: "Lead", people: 1, missing: [] },
  ],
  people_without_role: 2,
};

const permissionsReadiness = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    permissionsReadiness: (...args: unknown[]) => permissionsReadiness(...args),
  },
  ApiError: class ApiError extends Error {
    readonly status: number;
    constructor(status: number, message: string) {
      super(message);
      this.status = status;
    }
  },
}));

/** Mounts the panel for the store and waits for its roles. */
async function mount() {
  render(() => <PermissionsReadinessPanel tenant={TENANT} store={STORE} />);
  await waitFor(() => expect(screen.getByText("Cashier")).toBeTruthy());
}

/** The list item a role's name is in. */
function roleItem(name: string): HTMLElement {
  return screen.getByText(name).closest("li")!;
}

describe("before you turn this on", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    setLocale("en");
    permissionsReadiness.mockResolvedValue(READINESS);
  });
  afterEach(() => {
    cleanup();
    setLocale("en");
  });

  it("names each role's gaps in words, everyday ones first, with a link to edit the role", async () => {
    await mount();
    expect(permissionsReadiness).toHaveBeenCalledWith(TENANT, STORE);
    expect(screen.getByRole("heading", { name: "Before you turn this on" })).toBeTruthy();

    const cashier = roleItem("Cashier");
    expect(within(cashier).getByText(/3 people/)).toBeTruthy();
    const lines = within(cashier)
      .getAllByText(/^(Everyday|Audited|High risk)$/)
      .map((label) => label.parentElement?.textContent);
    expect(lines).toEqual([
      "Everyday: Send items to the kitchen",
      "Audited: Reprint a receipt",
      "High risk: Void a bill",
    ]);
    const edit = within(cashier).getByRole("link", { name: "Edit Cashier" });
    expect(edit.getAttribute("href")).toBe(`/t/${TENANT}/people?role=${CASHIER}`);

    expect(within(roleItem("Lead")).getByText("Grants everything the till asks for.")).toBeTruthy();
    expect(
      screen.getByText("2 people at this store hold no role, and could do nothing once this is on."),
    ).toBeTruthy();
    expect(screen.queryByText("Every role here grants all the everyday permissions.")).toBeNull();
  });

  it("says plainly when no role misses an everyday permission", async () => {
    permissionsReadiness.mockResolvedValue({
      ...READINESS,
      roles: [{ ...READINESS.roles[0]!, missing: [READINESS.roles[0]!.missing[2]!] }],
      people_without_role: 0,
    });
    await mount();
    expect(
      await screen.findByText("Every role here grants all the everyday permissions."),
    ).toBeTruthy();
    expect(screen.queryByText(/hold no role/)).toBeNull();
  });

  it("says the same in Vietnamese", async () => {
    setLocale("vi");
    await mount();
    expect(screen.getByRole("heading", { name: "Trước khi bật" })).toBeTruthy();
    const lines = within(roleItem("Cashier"))
      .getAllByText(/^(Thường ngày|Cần lưu vết|Rủi ro cao)$/)
      .map((label) => label.parentElement?.textContent);
    expect(lines[0]).toBe("Thường ngày: Gửi món xuống bếp");
  });

  it("says when the roles cannot be read", async () => {
    permissionsReadiness.mockRejectedValue(new ApiError(503, "the people service is unavailable"));
    render(() => <PermissionsReadinessPanel tenant={TENANT} store={STORE} />);
    expect(
      await screen.findByText(
        "Could not read the roles at this store: the people service is unavailable",
      ),
    ).toBeTruthy();
  });
});
