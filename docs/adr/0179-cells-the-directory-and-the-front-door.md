# ADR-0179 — Cells, the directory and the front door

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-10-26 · Supersedes in part
[ADR-0011](0011-country-in-hostname.md) (a hostname label means a cell id, and redirects are
302/307/308, never 301) and [ADR-0023](0023-tenant-hostname-and-slug.md) (at levels 0 and 1) ·
Relates to [ADR-0044](0044-fork-and-deploy.md), [ADR-0047](0047-minisign-verification.md),
[ADR-0050](0050-activation-code-exchange.md), [ADR-0067](0067-multi-admin-console-rbac.md),
[ADR-0114](0114-region-is-required-recorded-visible.md),
[ADR-0153](0153-a-vendor-is-a-provider-the-cloud-chooses.md)

The owner approved this decision on 2026-10-22 (D-R3, with the supersessions of ADR-0011 and
ADR-0023 and the directory key below). Data residency, and the owner's D-R1 and D-R2, are the
subject of ADR 0178, *Data residency and cross-border transfer, per country*, which is still
Proposed. This record makes no legal statement.

## Context

[ADR-0011](0011-country-in-hostname.md) gives each country a **cell**, a complete cloud deployment,
and puts the country in the hostname, with a slug → country directory answering `301`.
[ADR-0023](0023-tenant-hostname-and-slug.md) keeps "redirect, never proxy" but makes the tenant a
flat subdomain, with Cloudflare DNS as the slug-uniqueness ledger and wildcard certificates by
DNS-01. Neither is built: today one deployment serves every store, and nothing tells a device,
browser or integrator which deployment holds a tenant. Three problems with the recorded designs:

- A country is not a deployment. A second Vietnamese cell, or one cell serving two countries while
  a second is set up, has no name under ADR-0011.
- A `301` is cached by browsers without limit. A tenant that later moves cell is stranded.
- ADR-0023 makes Cloudflare, and DNS-01 with its API token, a dependency of creating a tenant,
  which a fork running on open-source parts alone cannot meet.

## Options considered

**A. Per-country hostname and `301`** (ADR-0011 as written). Simple, but names a country rather
than a deployment, and a cached `301` cannot be taken back.

**B. Cell id, a signed directory, and `302`/`307`/`308`.** Each cell is named by an id (`vn1`,
`jp1`). Every cell holds the same signed, PII-free directory and can answer "which cell holds this
tenant" on its own. Redirects are temporary unless a move is final.

**C. One global proxy in front of every cell.** One hostname for everyone, but every request
terminates TLS in the proxy's country. That is the transit ADR-0011 exists to avoid, and the proxy
is a global single point of failure.

**D. GeoDNS as the router of record.** Steers by the visitor's location, which is not where the
tenant's data lives: a Vietnamese owner travelling in Japan would land in the Japanese cell.
Needs a GeoIP-capable DNS provider for every fork.

## Decision

**Option B.**

### Three principles

1. **Every cell answers "where is this tenant".** It holds the directory and redirects; it never
   proxies another cell's traffic.
2. **GeoDNS and providers only optimise.** They may shorten the path to a cell for latency. They
   never decide which cell holds a tenant.
3. **Levels 0 and 1 run on open source only**: Caddy with HTTP-01 per-host certificates (no
   wildcard), PostgreSQL, Garage, minisign, and the registrar's plain DNS.

### Four levels

| Level | Shape | Needs |
|---|---|---|
| 0 | One cell. The default. A one-entry directory answers for itself | Nothing beyond today's stack |
| 1 | N cells, no GeoDNS. Each cell's host is a plain DNS record at the registrar | One directory, signed, in every cell |
| 2 | Level 1 plus self-hosted GeoDNS (PowerDNS with GeoIP) for latency | A DNS server the fork runs |
| 3 | Level 1 plus a managed GeoDNS or traffic provider | An adapter, or DNS configuration only |

### The rules (D-R3)

- **Naming.** A cell is named by a cell id (`vn1`, `jp1`), never by a country. A country alias
  (`vn` → `vn1`) is optional. Where a hostname carries a label, the label is a cell id.
- **Directory.** Every cell carries one directory, signed with minisign, with three maps:
  `tenant_slug → cell_id`; `country → default cell`, with a `*` fallback; and
  `cell_id → base URL`. It holds no personal data. It ships with a release, with configuration, or
  through Garage. Its signing key is its own, never the release key, and is held by the owner or a
  named delegate.
- **Redirects.** `302` for a browser; `307` for an API call, or `308` once a move is final. Never
  `301`.
- **Devices.** A device resolves its cell once: from the activation-code prefix (`VN1-…`), or from
  `/.well-known/pos-cell`, a JSON answer. It then stores `cloud_url`. If its tenant moves, a signed
  "cell moved" notice reaches it through the config pull.
- **Single cell.** A one-entry directory answers for itself, so level 0 needs no directory work.
- **Home cell.** A tenant's home cell is chosen from the store's or tenant's country, is sticky,
  and never follows the visitor's location.
- **GeoDNS** is optional and for latency only, as self-hosted PowerDNS GeoIP or a managed
  provider, set up as an adapter or as DNS.
- **Failover** stays within the same country.
- **Cookie.** The home-cell cookie holds only `cell_id`, and is host-only: never set on the parent
  domain.
- **Deploy.** Each cell has a non-secret `deploy/cells/<cell>.env` and one GitHub Environment. A
  matrix deploy runs the cells in canary order.
- **Region recording.** Backups and vendor connections record their region or processing country,
  and warn when it differs from the cell's.
- **HQ reporting** federates rollups through `/v1` with the `read_rollups` scope. Raw events never
  cross cells.
- **Console accounts** are per cell. OIDC single sign-on comes later.

### Partial supersessions

- **ADR-0011**: the hostname label means a cell id, not a country, and redirects are
  `302`/`307`/`308` instead of `301`. "Redirect, never proxy" and "cells do not know about each
  other's data" stand.
- **ADR-0023**, at levels 0 and 1: each host gets its own HTTP-01 certificate instead of a wildcard
  by DNS-01, and the directory's `tenant_slug → cell_id` replaces Cloudflare DNS as the slug ledger.
  Flat tenant names and "never set a cookie on the parent domain" stand.

### Implementation plan

Nothing is built by this record. Each item is its own slice, in this order where a prerequisite
says so.

| Item | What | Prerequisite |
|---|---|---|
| C1 | A `[cell]` section in `cloud.toml`: cell id, optional country alias, home country, base URL, where it runs. The console shows it | None |
| C2 | The directory: its file, minisign check against the directory key, and single-cell self mode | C1; the owner holds the directory key |
| C3 | `GET /.well-known/pos-cell`, a public JSON answer naming this cell and, for a tenant, its home cell | C2 |
| C4 | The activation-code cell prefix (`VN1-…`). It changes ADR-0050 and `pos-core`'s `ActivationCode` | Its own ADR, approved; C3 |
| C5 | The front door: a tenant homed elsewhere gets `302` (browser) or `307`/`308` (API), and the host-only `cell_id` cookie | C2 |
| C6 | A store or `locale` whose country differs from the cell's: a warning and an audited acknowledgement, never a refusal (D-R1 in ADR 0178) | C1 |
| C7 | Region or processing country on each backup destination and vendor connection, with the mismatch warning | C1 |
| C8 | `deploy/cells/<cell>.env`, a GitHub Environment per cell, and the canary-ordered matrix deploy. The `.github/` part is a draft the owner applies | C1 |
| C9 | The signed "cell moved" config node. It changes `pos-proto` | Its own ADR, approved; C2 |
| C10 | HQ federation of rollups across cells through `/v1` `read_rollups` | C2 |

## Consequences

- No code, port, protocol or dependency changes with this record. C4 and C9 each need an approved
  ADR before code; C9 moves `pos-proto`.
- Level 0 is today's single deployment plus one `[cell]` section. A fork that never adds a second
  cell does no more than that.
- Creating a tenant in a multi-cell fleet needs a signed directory update, so the directory key
  holder is on the path of every new tenant. That is the price of having no shared database and no
  DNS provider as the ledger.
- A stale directory in one cell redirects to the old home until it is refreshed. A `302` or `307`
  is not cached for long, so the fix is a directory update, not a client reset.
- Without GeoDNS, a visitor far from the home cell takes one redirect and the round trip. The
  cloud is off the sales path, so only the console feels it.
- `docs/architecture.md` §7, the glossary and `deploy/README.md` describe this design as decided,
  not built, until C1, C2 and C5 land.
