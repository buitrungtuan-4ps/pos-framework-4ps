// What a role grants directly and with approval, as the role editor holds it (ADR-0158 decision 4).
//
// The server keeps two lists that never overlap. The editor keeps one ticked set and the part of it
// granted with approval, and these are the four moves between the two shapes: read a stored role,
// tick or untick a permission, choose how a ticked one is granted, and write both lists back.

import { describe, expect, it } from "vitest";

import type { PermissionInfo, RoleTemplate } from "../src/api/types";
import { grantLists, grantsOf, NO_GRANTS, setApproval, toggleGrant } from "../src/lib/role-grants";

function permission(id: string, pinRequired: boolean): PermissionInfo {
  return { id, group: "BILLING", risk: "HIGH", pin_required: pinRequired, description: id };
}

const VOID_BILL = permission("billing.bill.void", true);
const TAKE_PAYMENT = permission("billing.payment.take", false);

function role(permissions: string[], withApproval?: string[]): RoleTemplate {
  return {
    role_template_id: "01ROLE",
    tenant_id: "01TENANT",
    name: "Cashier",
    permissions,
    permissions_with_approval: withApproval,
    status: "active",
    etag: "1",
  };
}

describe("role grants", () => {
  it("reads a stored role as everything it grants, and which of it with approval", () => {
    const grants = grantsOf(role(["billing.payment.take"], ["billing.bill.void"]));
    expect(grants.granted).toEqual(["billing.payment.take", "billing.bill.void"]);
    expect(grants.withApproval).toEqual(["billing.bill.void"]);
    // A server too old to send the second list grants nothing with approval.
    expect(grantsOf(role(["billing.bill.void"])).withApproval).toEqual([]);
  });

  it("starts a PIN-flagged permission with approval when ticked, and any other directly", () => {
    // The catalogue's PIN flag is the default a role starts from (ADR-0158 decision 4).
    const ticked = toggleGrant(toggleGrant(NO_GRANTS, VOID_BILL, true), TAKE_PAYMENT, true);
    expect(grantLists(ticked)).toEqual({
      permissions: ["billing.payment.take"],
      permissionsWithApproval: ["billing.bill.void"],
    });
  });

  it("grants a ticked permission directly when the operator says so, and back again", () => {
    const direct = setApproval(toggleGrant(NO_GRANTS, VOID_BILL, true), VOID_BILL.id, false);
    expect(grantLists(direct)).toEqual({
      permissions: ["billing.bill.void"],
      permissionsWithApproval: [],
    });
    expect(grantLists(setApproval(direct, VOID_BILL.id, true)).permissionsWithApproval).toEqual([
      "billing.bill.void",
    ]);
  });

  it("forgets how a permission was granted once it is unticked", () => {
    const withdrawn = toggleGrant(grantsOf(role([], ["billing.bill.void"])), VOID_BILL, false);
    expect(withdrawn).toEqual({ granted: [], withApproval: [] });
    // Choosing how an unticked permission is granted grants nothing.
    expect(setApproval(withdrawn, VOID_BILL.id, true)).toEqual(withdrawn);
  });

  it("keeps how a permission is granted when it is ticked again while already ticked", () => {
    const direct = grantsOf(role(["billing.bill.void"]));
    expect(toggleGrant(direct, VOID_BILL, true)).toBe(direct);
  });

  it("writes back two lists that never share a permission", () => {
    const lists = grantLists(grantsOf(role(["billing.payment.take"], ["billing.bill.void"])));
    const shared = lists.permissions.filter((id) => lists.permissionsWithApproval.includes(id));
    expect(shared).toEqual([]);
  });
});
