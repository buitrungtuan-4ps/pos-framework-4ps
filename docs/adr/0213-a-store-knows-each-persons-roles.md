# ADR-0213 — A store knows each person's roles, so a role's screens and home resolve on the till

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-12
· Extends [ADR-0070](0070-people-and-access.md) and
[ADR-0158](0158-the-till-enforces-each-persons-own-permissions.md) (what the `permissions` node carries)
· Relates to [ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md),
[ADR-0190](0190-a-screen-is-drawn-from-the-package.md),
[ADR-0210](0210-a-role-names-its-home-screen-in-the-package.md)

The owner approved it on 2026-11-12, after phase 4 found the gap.

## Context

Role is one of the six dimensions a package resolves by (ADR-0189), and ADR-0210 lets a version name
a home screen per role. But the published `permissions` node gives each member of staff their
permission ids, not their roles: `PublishedStaffMember` has no role field. So neither the edge nor the
till can tell which role a signed-in person holds. Today a role's screen variant and a role's home
both fall back to the base on every store, and HQ's per-role designs never reach a till.

## Options considered

1. **Publish each person's role ids on the staff member** (chosen). Additive, and the cloud already
   knows the assignments it compiles permissions from.
2. **Resolve role variants in the cloud per person** and publish a version per person. Multiplies the
   published document by the staff count and changes the package's shape.
3. **Infer the role from the permission set.** Two roles with the same permissions become one, and a
   change to a role's permissions silently changes which screens its people see.

## Decision

1. **`PublishedStaffMember.role_ids`**: the ids of the roles the person holds at that store, as ULID
   strings, sorted and without duplicates. Additive, skipped from the wire when empty, so a node
   compiled before the field, or for a person with no role, is byte-identical. An edge that predates
   it ignores it.
2. **The edge answers the signed-in person's role ids** on `GET /api/session` (`role_ids`), from the
   roster it already authorises the sign-in against. No new route.
3. **The till resolves a role variant, and a role's home, with those ids.** A variant whose selector
   names a role matches a person who holds that role. When several of a person's roles match, the
   variant listed first in the version wins, so the result does not depend on the order of the
   person's roles. With no match, the base applies, as today.
4. **The tap count of ADR-0210 is unchanged**: it already counts per role the version names.
5. **Personal data.** A role id is an opaque identifier of a job function, published in the same node
   that already carries the person's name and code (T1). It is never put in an event or a log, and
   the console shows it only where it already shows the person's roles.

## Consequences accepted

- An additive `pos-proto` field and an additive session field, with the owner's review of
  `pos-proto`, and a `min_edge_version` floor for a version whose role variants or homes should
  resolve per person (an older edge still runs it, with the base for everyone).
- The cloud's people compiler writes the field, and its contract tests cover it.
- A role change reaches the till with the next `permissions` publish, like a permission change today.
