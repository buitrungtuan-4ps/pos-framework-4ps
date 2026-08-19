# ADR-0023 — Tenant hostnames: flat per-tenant subdomains, DNS as the uniqueness ledger

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-19
**Relates to** [ADR-0011](0011-country-in-hostname.md) · [ADR-0003](0003-cattle-not-pets.md)
**Resolves** spec-gap: ADR-0011 and the frozen archive contradict on the tenant hostname model.

**Context.** [ADR-0011](0011-country-in-hostname.md) established the sound half — **redirect, never
proxy**: a hostname resolves to exactly one cell, so a request must land at the right cell directly
rather than be forwarded from a home country. But it went on to put the **country** in the hostname
with a slug→country directory issuing `301`s, while the frozen archive (`kien-truc` §16.4) specifies
something different and more operable:

- **flat per-tenant names with no visible country label** — `<slug>.<base-domain>`;
- **per-tenant DNS records created through the Cloudflare API** as a tenant is provisioned;
- **DNS itself as the global slug-uniqueness ledger** — there is no shared database between cells, so
  "is this slug taken?" is answered by trying to create the record;
- and a cert note: above ~5 cells, **stagger wildcard renewals** to stay under Let's Encrypt's
  duplicate-certificate ceiling.

Both agree on redirect-never-proxy; they disagree on where the country lives and on what adjudicates
slug uniqueness. The archive's README says a rule present only in the archive is a bug in the English
set, so the archive wins on the points of conflict.

**Decision.**

- **A tenant is a flat subdomain, `<slug>.<base-domain>`, with no country segment.** The country is a
  property of the tenant's record and its cell, not a label in the host. A device or browser reaches a
  tenant at its own name and is served by the one cell that name resolves to.
- **DNS is the slug-uniqueness ledger.** Provisioning a tenant creates its DNS record through the
  Cloudflare API; **the creation succeeding *is* the uniqueness check** — a slug already in DNS cannot
  be created twice, so no shared cross-cell database is needed to arbitrate names. A failed creation is
  a taken slug, surfaced as such.
- **Redirect, never proxy** (retained from [ADR-0011](0011-country-in-hostname.md)). A name that must
  move to another cell answers with a redirect; no cell forwards another cell's traffic. Session
  cookies are per-subdomain and **never** set on the parent domain — the single worst multi-tenant
  isolation failure.
- **Certificates: stagger wildcard renewals above ~5 cells.** Each cell terminates TLS for the tenant
  names it serves; beyond roughly five cells the renewals are spread over time so the fleet stays under
  Let's Encrypt's duplicate-certificate rate ceiling. DNS-01 is the preferred challenge (records are
  already API-managed); **"Flexible" SSL is forbidden** (P8).
- **No purchased domain is not a blocker.** `DOMAIN=<vps-ip>.sslip.io` gives a working HTTPS name for a
  fork with no domain of its own, so `fork-and-deploy` (P8) reaches a live admin UI without buying
  anything.

**Consequences.**

- Tenant provisioning (P7) calls the Cloudflare API to create the record and treats the API's
  uniqueness failure as the authoritative "slug taken"; there is no `slugs` table to keep consistent
  across cells.
- This supersedes ADR-0011's country-in-hostname mechanism (the slug→country directory and its `301`s);
  ADR-0011's redirect-never-proxy principle is retained and restated here.
- The cell that serves a tenant is chosen at provisioning and encoded in the DNS target; moving a
  tenant between cells is a DNS change plus a redirect during the cut-over, not a proxy.
- Cloudflare is now an operational dependency of provisioning. A fork using `sslip.io` and manual DNS
  can skip the API integration; the provisioning code isolates the DNS step behind a small trait so the
  Cloudflare call is one implementation, not a hard-wired assumption.
