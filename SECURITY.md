# Security policy

**Status** Accepted · **Owner** @maintainers-security · **Last reviewed** 2026-08-18

## Reporting a vulnerability

Report privately to the maintainers listed in [`MAINTAINERS.md`](MAINTAINERS.md). Do **not** open a public issue, and do not include customer data in the report. Expect an acknowledgement within two working days and an assessment within five.

## What we consider high severity

- Anything that lets one tenant read or write another tenant's data (row-level security bypass, session or cookie scope errors).
- Anything that lets an unauthenticated caller create, modify, or settle a bill.
- Anything touching the update path: forged artifact signatures, tampering with the release manifest, or obtaining a signing key.
- Server-side request forgery through webhook endpoints, or leaking internal addresses.
- Personal data exposure: PII in logs, in event payloads, or served to the wrong tenant.

## Boundaries that must never be crossed

1. **Signing keys never touch CI or a server.** Releases are signed manually from an offline key ([ADR-0009](docs/adr/0009-licence.md), engineering-guide §6).
2. **Operational secrets are generated on the host** and never travel back to GitHub (engineering-guide §11).
3. **PII lives outside the event log**, referenced by `subject_id`, so erasure never requires rewriting history.
4. **Agents and automation run without secrets** and cannot merge.

## Handling a suspected breach

Pause the affected integration or endpoint (endpoints can be disabled per tenant), preserve logs and audit records, notify the technology lead, and only then investigate. Data-protection notification duties depend on jurisdiction and are decided by the operator of the deployment, not by this repository.
