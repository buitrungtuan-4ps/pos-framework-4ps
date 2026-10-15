# `pos-cloud-api`

**Layer** `app` · **Owners** @maintainers-cloud

## What it is

What every cloud route module stands on, whichever business area it serves: who the caller is and what they may do, the audit trail a change writes to, the page every list returns, and the one error envelope every refusal sends. It holds no routes and no application state. `pos-cloud` composes it and re-exports `auth`, `audit`, `paging` and `version` under their old paths.

## Owns

- Console authentication: super-admin enrolment, passwords, TOTP, sessions, invites, roles and `ConsolePermission`
- API keys and device credentials, and the bearer check that reads them
- The audit trail's types and store trait
- Page requests and pages, the paging params a list route reads and the page it answers with
- A record's version, `If-Match`, and the responses that carry a version (optimistic concurrency)
- The AIP-193 error envelope, the field-level refusals built on it, the refusals every area's routes share (`not_found`, `enum_refusal`, `FieldRefusal` and the rest), its schema for the API documents, and the CSV download response
- What a console route does around its work: the permission check, the tenant query, the audit entry
- The formula rule every CSV file keeps
- Its stores over PostgreSQL: the admin, API-key and audit tables, through `store-postgres`

## Depends on / used by

- **Depends on** (workspace crates): `pos-ports`, `pos-proto`, `store-postgres`; in tests, `pos-fakes`.
- **Used by** (workspace crates): `pos-cloud`, `pos-cloud-catalog`, `pos-cloud-config`, `pos-cloud-devices`, `pos-cloud-floor`, `pos-cloud-inventory`, `pos-cloud-reports`.

## Entry points

- `src/auth/`: `admin` (sessions, roles), `console_rbac` (`ConsolePermission`), `apikey` and `bearer`, `device_credential`
- `src/audit.rs`: `AuditRecorder`, `AuditStore`, `AuditEntry`
- `src/paging.rs`: `PageRequest`, `Page`; `src/paged.rs`: `parse_page`, `paged_ok`
- `src/version.rs`: `Version`, `Versioned`, `UpdateOutcome`, `CreateOutcome`; `src/conditional.rs`: `if_match`, `versioned_ok`, `version_mismatch`
- `src/response.rs`: `api_error`, `parse_ulid_fields`, `csv_download_response`; `src/refusal.rs`: `not_found`, `enum_refusal`, `FieldRefusal`, `parse_optional_ulid`
- `src/console.rs`: `require_permission`, `audit_action`, `ConfigTenantQuery`
- `src/csv_file.rs`: `escape_formula`, `unescape_formula`
- `src/error_schema.rs`: the `ErrorResponse` schema

## Rules

- [ADR-0034](../../docs/adr/0034-super-admin-auth.md): super-admin authentication
- [ADR-0067](../../docs/adr/0067-multi-admin-console-rbac.md): console roles and permissions
- [ADR-0094](../../docs/adr/0094-console-optimistic-concurrency.md) and [ADR-0095](../../docs/adr/0095-conditional-writes-for-collections.md): a record's version and conditional writes
- [ADR-0098](../../docs/adr/0098-paged-admin-reads.md): paged reads
- [ADR-0026](../../docs/adr/0026-port-shapes.md): one failure type, the AIP-193 `ErrorStatus` the error envelope carries
- [ADR-0170](../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4: why this is a crate

## How to test

```bash
cargo test -p pos-cloud-api
```
