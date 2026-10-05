// What a role grants, as the role editor holds it (ADR-0158 decision 4).
//
// A role grants each permission either directly, which its holder acts on alone, or with approval,
// where another person who holds it directly enters their code and PIN for each act. The server
// keeps the two as separate lists that never overlap, `permissions` and
// `permissions_with_approval`. The editor holds them as one ticked set and the part of it granted
// with approval, because that is the shape of the form: a checkbox per permission, and for a
// PIN-flagged one a choice of how.

import type { PermissionInfo, RoleTemplate } from "../api/types";

/** Every permission a role grants, and which of them it grants only with approval. */
export interface RoleGrants {
  readonly granted: readonly string[];
  readonly withApproval: readonly string[];
}

/** A new role, which grants nothing. */
export const NO_GRANTS: RoleGrants = { granted: [], withApproval: [] };

/** A stored role as the editor holds it. */
export function grantsOf(role: RoleTemplate): RoleGrants {
  const withApproval = role.permissions_with_approval ?? [];
  return {
    granted: [...new Set([...role.permissions, ...withApproval])],
    withApproval: [...withApproval],
  };
}

/**
 * Ticks or unticks one permission. A PIN-flagged permission starts with approval when it is ticked,
 * because the catalogue's PIN flag is the default a role starts from and nothing more; the operator
 * may then choose to grant it directly. Unticking forgets how it was granted.
 */
export function toggleGrant(grants: RoleGrants, info: PermissionInfo, on: boolean): RoleGrants {
  if (!on) {
    return {
      granted: grants.granted.filter((id) => id !== info.id),
      withApproval: grants.withApproval.filter((id) => id !== info.id),
    };
  }
  if (grants.granted.includes(info.id)) {
    return grants;
  }
  return {
    granted: [...grants.granted, info.id],
    withApproval: info.pin_required ? [...grants.withApproval, info.id] : grants.withApproval,
  };
}

/** Grants one ticked permission with approval, or directly. One not ticked is left alone. */
export function setApproval(grants: RoleGrants, id: string, approval: boolean): RoleGrants {
  if (!grants.granted.includes(id)) {
    return grants;
  }
  const others = grants.withApproval.filter((existing) => existing !== id);
  return { granted: grants.granted, withApproval: approval ? [...others, id] : others };
}

/** The two lists the role routes take: what is granted directly, and what only with approval. */
export function grantLists(grants: RoleGrants): {
  permissions: string[];
  permissionsWithApproval: string[];
} {
  return {
    permissions: grants.granted.filter((id) => !grants.withApproval.includes(id)),
    permissionsWithApproval: grants.granted.filter((id) => grants.withApproval.includes(id)),
  };
}
