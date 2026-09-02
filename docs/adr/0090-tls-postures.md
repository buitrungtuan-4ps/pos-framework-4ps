# ADR-0090 — TLS termination is a fork-level posture, chosen explicitly

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-02
**Relates to** [ADR-0044](0044-fork-and-deploy.md) (what a fork configures and what the box mints) · [ADR-0023](0023-tenant-hostname-and-slug.md) (redirect, never proxy, never Flexible) · [ADR-0067](0067-multi-admin-console-rbac.md) (the `/admin/login` rate limit whose key this makes configurable) · [ADR-0089](0089-edge-event-bus-transport.md) (the event bus that waits on the certificate path this establishes) · `docs/roadmap-v3.md` (debate D24)

**Context.** The deployment has one TLS mechanism and three problems with it.

**It infers the posture from a hostname suffix.** [`bootstrap.sh`](../../deploy/bootstrap.sh) matches
`DOMAIN` against `*.sslip.io` and picks ACME HTTP-01; anything else with a non-empty
`CF_DNS_API_TOKEN` picks DNS-01 through Cloudflare; and anything else with an **empty** token falls
through to HTTP-01 without comment. That last branch is a misconfiguration engine. An operator who
forgot the token — or whose fork's Environment simply does not define that secret, so the workflow
passes an empty string — gets a cell attempting HTTP-01 against a grey-clouded record where nothing
inbound on `:80` can reach ACME. The only symptom is a missing certificate, and the bootstrap log
says it chose HTTP-01 *on purpose*.

**Two of the four legitimate postures have no path at all.** ACME HTTP-01 and ACME DNS-01 are
served. A company with a wildcard certificate from its own internal CA and no intention of using
ACME is not; neither is a company whose F5, nginx, or Cloudflare Tunnel already terminates TLS and
where the bundled proxy should terminate none. Both must patch the repository, and for a framework
meant to be forked, "patch the repository" is the same as unsupported.

**The swap overwrites a version-controlled file.** The mechanism is
`cp Caddyfile.cloudflare Caddyfile`. Two costs, neither about TLS: after a bootstrap the file on disk
no longer matches the file in git, and **nothing on the box records which posture it is in** — both
modes end up in a file named `Caddyfile`, so "which TLS mode is this cell?" is answerable only by
diffing content against the repository.

And one dependency waits on this. [ADR-0089](0089-edge-event-bus-transport.md) sequenced the event
bus behind this record because TLS on the bus needs **a certificate at a path some posture put there
on purpose**, rather than a reach into Caddy's private volume layout with no renewal hook — the exact
arrangement whose failure mode is a silent expiry sixty to ninety days after nobody noticed.

**Decision.**

- **`TLS_MODE` is explicit, has exactly four values, and nothing is inferred from `DOMAIN` any
  more.** `acme-http01` · `acme-dns01` · `byo-cert` · `external`. An absent or empty `TLS_MODE` means
  `acme-http01`, so an existing cell — including an sslip.io pilot — is unchanged by this record and
  needs no new secret. `DOMAIN` goes back to meaning only "the hostname this cell answers on".

- **Each mode's required inputs are checked at bootstrap and refused loudly when absent.**
  `acme-dns01` without `CF_DNS_API_TOKEN` is now an error that stops the run, not a silent downgrade
  to a method that cannot work on a DNS-only record. `byo-cert` with no certificate files present is
  an error too, and stops before Compose starts a Caddy that would fail to load them.

- **Nothing overwrites a committed file.** Each mode is a complete, committed Caddyfile under
  `deploy/Caddyfile.d/`; `bootstrap.sh` installs the selected one as the **generated**
  `secrets/Caddyfile` and Compose mounts that. The shared part — compression and the
  health-checked `reverse_proxy` to `pos_cloud:8080` — lives in **one** committed file the four
  import, so the proxy configuration exists once rather than in four copies that must be edited in
  lockstep. The repository's own tree stays pristine; `git status` on the box stays clean.

- **The chosen mode is recorded on the box** in `secrets/caddy.env` beside `DOMAIN`, and printed by
  bootstrap on every run. A cell can be *asked* which posture it is in, which is what the runbook,
  an incident, and the next operator all need.

- **`secrets/tls/` is the canonical certificate location in every mode, and each mode says who
  populates it** — `fullchain.pem` and `privkey.pem`, one pair, one place:
  - `byo-cert` — the **operator** installs them, and Caddy serves them directly with no ACME.
  - `acme-http01` and `acme-dns01` — **Caddy** issues into its own `caddy_data` volume as today, and
    an exporter republishes the result here.
  - `external` — **nobody**. There is no certificate on this box; the directory stays empty.

- **The ACME export is one script with one job and a loud failure**, `deploy/tls-export.sh`,
  installed by bootstrap and run from cron. It reads the certificate **through the container**
  (`docker compose exec caddy`), so it needs neither root nor any knowledge of where Docker keeps
  volume data; it resolves the ACME-directory path segment by glob and **fails when the glob matches
  zero or more than one** rather than picking one; it writes only when the fingerprint changed; and
  it signals the consumers it updated. That is precisely the renewal hook ADR-0089 found missing, and
  confining it to one small script with an exit status is the price of not mounting one service's
  private volume into a third container.

- **`external` changes the application, not only the proxy**, in two ways.

  Caddy stops offering `443` to the internet. The publish becomes **loopback-bound rather than
  removed** — Compose cannot conditionally omit a port, and a `127.0.0.1` publish is the honest
  equivalent — and the site is served as plain HTTP for the upstream terminator to reach, on a
  publishable port so a box that already runs something on `:80` can still be used.

  And **`TRUSTED_PROXY_HOPS` becomes configuration**: `pos_cloud` gains a `trusted_proxy_hops` field
  (default `1`, today's constant), which bootstrap sets to `2` in this mode, because the chain is
  then `client, upstream-balancer, caddy` and the client is two back. Without it the `/admin/login`
  rate limit — [ADR-0067](0067-multi-admin-console-rbac.md) slice 5, repaired earlier in this same
  release — keys every request on the balancer's single address: **every admin in the company shares
  one bucket, and one person's wrong passwords lock out the rest.** That is the hidden edge D24
  named, and it is a correctness change in the application rather than a deployment detail.

- **The Compose variables live in a generated `deploy/.env`, not only in bootstrap's own
  environment.** Compose reads `.env` beside the file automatically, so a later
  `docker compose up -d` typed by hand reproduces the same posture. Passing them only through
  bootstrap's environment would mean the published ports **silently revert to the defaults** the next
  time anyone brought the stack up without it — internet-facing `443` quietly reappearing on a cell
  whose operator believes TLS terminates upstream. That is this program's recurring failure shape,
  and avoiding it costs one generated file.

**What each mode actually changes.**

| `TLS_MODE` | Caddy image | Certificate from | Published | `secrets/tls/` | `trusted_proxy_hops` | Also requires |
|---|---|---|---|---|---|---|
| `acme-http01` (default) | stock | ACME HTTP-01 / TLS-ALPN | `80`, `443`, `443/udp` | exporter | 1 | `:80` reachable from the internet |
| `acme-dns01` | custom (`caddy-dns/cloudflare`) | ACME DNS-01 | `80`, `443`, `443/udp` | exporter | 1 | `CF_DNS_API_TOKEN`, a Cloudflare-managed zone |
| `byo-cert` | stock | the files the operator installed | `80`, `443`, `443/udp` | the operator | 1 | `fullchain.pem` + `privkey.pem` present |
| `external` | stock | nothing on this box | HTTP only; `443` loopback-bound | empty | 2 | an upstream terminator that sets `X-Forwarded-*` |

`byo-cert` keeps `:80` published for the HTTP→HTTPS redirect, not for issuance.

**Why not keep inferring from `DOMAIN`.** Inference cannot express two of the four postures at all —
"I have my own certificate" and "something else terminates" are not properties of a hostname, so no
amount of suffix matching reaches them. It cannot distinguish *"empty token, sslip.io, correct"* from
*"empty token, managed domain, broken"*, which is the fallthrough above. And it makes the decision
implicit, so nothing records it and nothing can validate it. Explicitness costs exactly one variable
whose default preserves current behaviour.

**Deliberately deferred (flagged, not silently dropped).**

- **A stale export is not alerted on.** The exporter fails to its own exit status and log; nothing in
  the alert engine ([ADR-0073](0073-alerting.md)) watches whether `secrets/tls/` has gone stale. On
  the ACME modes, an exporter that quietly stopped working reproduces exactly the silent-expiry
  failure this record set out to prevent — just further down the timeline. An alert rule over the
  exported certificate's `notAfter` is the follow-up, named here so it is a deferral and not an
  oversight.
- **Rotation under `byo-cert` is the operator's.** The runbook states the reload step; nothing
  automates it, because the source of those files is by definition outside this deployment.
- **`caddy validate` in CI over all four modes.** Worth having and not free: one mode needs the
  custom image with the Cloudflare plugin, so the check cannot be a single stock-binary invocation.
  Until it exists, a typo in a mode nobody currently deploys is found on the box.
- **`external` with client certificates from the upstream balancer.** A balancer that authenticates
  callers by certificate can only pass that identity as a header, and nothing here reads one. Out of
  scope, and it is the same limitation ADR-0089 records for the bus.
- **Whether the bus can use the exported certificate at all.** ADR-0089's implementation proves that
  on a real box. If the export turns out unusable there, the fallback is a private-CA server
  certificate for the bus, which costs `link-nats` a root-certificate option and one config field —
  named here because ADR-0089 states "no code change in `link-nats`", and that claim holds only on
  the exported-certificate path.

**Rejected.**

- **A fifth "plaintext, no TLS" mode for local development.** It would be operationally
  indistinguishable from `external` misconfigured — a cell serving plain HTTP to the internet, which
  is what `external` looks like when the upstream terminator is missing — and `external` already
  covers "something else terminates this". Local development runs the binary directly, not this
  Compose file. Four values, no fifth.
- **Four complete Caddyfiles with no shared import.** The health-checked `reverse_proxy` block is
  already duplicated across the two files that exist; four copies is the version that eventually
  diverges, and it diverges in the file for the mode nobody is watching.
- **Letting Caddy decide from `TLS_MODE` itself.** The Caddyfile has no conditionals; the nearest
  construct is a request matcher, which cannot switch a `tls` directive or a site address. Selecting
  a file at bootstrap is the mechanism Caddy actually offers.
- **Mounting `caddy_data` into every service that needs a certificate.** Rejected in ADR-0089 for
  coupling consumers to another service's private storage layout, and rejected again here: the
  exporter reads the same bytes through one documented seam, with a renewal hook and a failure
  status, instead of N containers each depending on a path whose ACME-directory segment can change.

**Consequences.**

- **A fork with its own certificate, or its own terminator, has a supported path** — which is the
  point. This is the second of the two posture records D23/D24 asked for, and the one that makes the
  deployment usable by a company that does not want ACME anywhere near its edge.
- **One new variable, whose default is today's behaviour.** An existing deployment redeploys
  unchanged and keeps its certificate; nothing rotates.
- **`acme-dns01` without a token now refuses to bootstrap.** A deployment that believed it was on
  DNS-01 while silently running HTTP-01 will stop until either the token is set or the mode is
  corrected to match reality. Refusing is right — the alternative is what it has been doing — but it
  is a behaviour change and belongs in the changelog as an upgrade note.
- **`pos_cloud` gains one configuration field** and `client_ip` reads it instead of a constant. No
  wire, protocol, `pos-proto`, permission, or migration change.
- **`secrets/tls/` exists and is populated on three of the four modes**, which is what unblocks
  ADR-0089's implementation.
- **One more thing runs from cron on the box** on the two ACME modes, alongside the backup and
  restore-drill jobs of [ADR-0046](0046-backups-and-restore.md).
