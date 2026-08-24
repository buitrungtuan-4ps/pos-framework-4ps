# Maintainers

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

| Area | Owner | Responsibilities |
|---|---|---|
| Architecture, ports, `pos-proto` | _TBD_ | Approves ADRs, protocol changes, port changes |
| Domain (`pos-core`) | _TBD_ | Business rules, state machines, invariants |
| Cloud (`pos-cloud`, deploy) | _TBD_ | Control plane, deployment, upgrades |
| Edge (`pos-edge`, devices) | _TBD_ | In-store runtime, printers, terminals |
| Release and keys | _TBD_ + one backup | Signs releases from the offline key; holds key A / key B |
| Security contact | _TBD_ | First responder for `SECURITY.md` reports |

> **Unfilled owners block enforcement, not just routing.** `CODEOWNERS` routes review to
> `@maintainers-architecture`, `@maintainers-domain`, `@maintainers-cloud` and
> `@maintainers-security`. GitHub silently ignores a CODEOWNERS entry naming a team that does
> not exist or cannot read the repository — the required-review protection on `pos-core`,
> `pos-ports`, `pos-proto` and `.github/` then does nothing at all, with no warning anywhere.
> Create the four teams and fill the table below before relying on branch protection.

**Rules**

1. At least **two** people must be able to sign a release and reach the offline keys. A single point of failure in release capability is itself an incident.
2. Every procedure lives in `docs/` or a runbook, never only in someone's head.
3. Maintainer changes are recorded here in the same pull request that changes access.
