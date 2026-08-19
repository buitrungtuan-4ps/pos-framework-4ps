# Engineering guide

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

How this repository is organised, tested, released, and deployed. Day-to-day rules for contributors are in [AGENTS.md](../AGENTS.md); this document explains the machinery behind them.

---

## 1. Repository strategy

One private monorepo (a Cargo workspace), because:

- The cloud and edges speak a shared protocol. Separate repositories invite silent version drift — the most expensive class of bug in a distributed system. One repository means one pull request changes both sides and CI tests them together.
- Changing a port touches several adapters; that belongs in **one atomic commit**, not a five-repository dance.
- One CI, one lint configuration, one `Cargo.lock`, reproducible builds.

Deployment manifests live in `deploy/` inside the same repository, because fork-and-deploy requires that forking *one* repository gives you everything. Sensitive access is controlled with GitHub Environments and CODEOWNERS, not by splitting repositories.

## 2. Branching

Trunk-based with short-lived branches. `main` is always releasable.

```
  feat/kds-bump ───┐
  fix/print-retry ─┼──► main ──► tag v1.4.0 ──► release/1.4.x
                   │                                │
                   └────────────────────────────────┴──► hotfix on the release branch,
                                                         then cherry-pick to main
```

Feature branches live under three days. **Release branches exist because updates roll out in rings**: when ring 1 is running 1.4.0 and `main` has moved on, an urgent fix must come from the tree actually deployed in stores. `main` and `release/*` are protected: no direct pushes, PR plus green CI plus one review, no force-push, no deletion.

Commits follow **Conventional Commits** with a crate scope — `feat(fiscal-vn): support adjusted invoices` — which generates the changelog and the version bump.

## 3. Versioning

Two independent axes, as described in [naming-and-api.md](naming-and-api.md) §11: the product SemVer and `PROTOCOL_VERSION`. The rules that matter operationally:

1. The cloud **must** support at least the two most recent protocol versions; CI has a test that proves it.
2. Protocol changes are additive. A breaking change bumps `PROTOCOL_VERSION` and runs both versions in parallel for at least two releases.
3. Migrations within a release may only add tables and columns, so adjacent versions can read one database — this is what makes automatic rollback safe.

## 4. Continuous integration

**On every pull request (target: under 10 minutes)**

```
fmt → clippy -D warnings → unit tests (core: no database, no network)
    → build matrix: linux-x86_64 + windows-x86_64
    → cargo-deny (licences, advisories, duplicates)
    → gitleaks (no secrets)
    → dependency rule test: pos-core / pos-ports allowlist only
    → naming linter: OpenAPI + SQL migrations + event and permission registries
    → snapshots: public API, event schema, permission catalogue
    → UI build
```

**On merge to main**

```
+ integration tests against real PostgreSQL and NATS service containers
+ contract tests: every implementation of every port runs the same suite
+ simulator smoke test: virtual stores, order flow, offline/online, reconciliation
```

**Nightly**

```
+ long-running simulator soak
+ restore drill: random store backup and the cloud database restored and verified
+ cargo-audit against new advisories
```

The two checks that turn architecture into law are the **dependency rule test** (nobody can accidentally import tokio into the domain) and **cargo-deny** (nobody can accidentally introduce copyleft).

### Keeping documentation true

Documentation rots silently, so three checks run in CI: **link checking** across `docs/` and the root markdown files; the **docs-touched gate** described above; and **generated-document drift** — the OpenAPI file, permission matrix, and event catalogue are regenerated and compared, so a hand-edit or a stale artefact fails the build.

Two habits carry the rest. Each document names an owner in `CODEOWNERS`, so review lands on someone accountable. And documents are written as **rules with reasons** rather than narrative: a reader should be able to check compliance without reconstructing the discussion that produced the rule.

## 5. Dependencies, licences, and independence

`deny.toml` allows MIT, Apache-2.0, ISC, BSD, and public-domain licences; warns on MPL-2.0 (currently only the serial-port crate, which is scheduled for replacement); and **hard-blocks AGPL, GPL, and SSPL** inside binaries. Duplicate versions and unknown sources are rejected.

**Vendoring is what makes "no external dependency" literal.** `cargo vendor` output is committed (or archived somewhere we control), so the repository builds offline forever — even if crates.io is unreachable or a crate is deleted, which has happened in other ecosystems. `rust-toolchain.toml` pins the compiler so a build today and a build in three years produce the same result.

Dependency updates arrive as weekly grouped pull requests and are never auto-merged.

**GitHub is also a vendor.** A daily job mirrors `main` and all tags to a second remote. Losing the GitHub account would cost convenience, not the asset.

## 6. Releases and signing

```
tag v1.4.0
   │
   ▼
CI builds both platforms, generates an SBOM, publishes hashes      ← the runner never holds a key
   │
   ▼
maintainer downloads artifacts and signs with the offline USB key (minisign)   ← deliberate manual step
   │
   ▼
signed artifacts and manifest uploaded to the release store
   │
   ▼
ring 0 (lab store) → observe → ring 1 → ring 2   (see architecture.md §4)
```

Manual signing with an offline key is a design decision, not missing automation: it guarantees there is no path by which a compromised pipeline can ship software to the entire fleet. When builds are fully stable this may become two signatures — a CI build key plus a maintainer promotion key — but the promotion key stays offline.

### Change history and release notes

Nothing ships without a written trail, because the people who need it most — an operator at 21:00, a contributor six months later — cannot ask the author.

| Artefact | Rule |
|---|---|
| **Commit** | Conventional Commits with a crate scope. This is what generates the changelog |
| **`CHANGELOG.md`** | Every user-visible change lands under `[Unreleased]` in the correct category, with the issue number, **in the same pull request** as the code |
| **Upgrade note** | Mandatory whenever `PROTOCOL_VERSION`, a migration, a permission identifier, or a default value changes. It states what breaks, what is safe, and whether rollback is safe |
| **Release notes** | Assembled per tag from the changelog plus upgrade notes. Header states product version, protocol version, MSRV. Includes one plain sentence describing what changes **for restaurant staff** |
| **Deprecations** | Listed in every release from the first announcement until removal, never fewer than two releases |
| **Hotfix** | Branches from `release/x.y`, is cherry-picked back to `main`, and appears in both release notes |

CI enforces the mechanical part: a pull request that changes `crates/**` or `ui/**` must also touch `CHANGELOG.md` or state `changelog: none` with a reason in the body; a pull request that changes behaviour must touch `docs/**` or state `docs: none needed`. Both gates are deliberately escapable with an explicit sentence — the goal is a conscious decision, not a bureaucratic block.

**Pre-tag checklist:** CI green · changelog generated · protocol backwards-compatible for two versions · migrations additive · simulator soak passed · rollback runbook re-read.

## 7. Testing strategy

| Layer | Runs | Speed | Content |
|---|---|---|---|
| Unit — pure domain | every PR | seconds | All business rules with in-memory fakes: **no database, no network, no hardware** |
| Port contract tests | every PR | tens of seconds | Every implementation of a port passes the same suite |
| Integration | on main | minutes | Real PostgreSQL and NATS in containers |
| Fleet simulator | on main and nightly | long | Virtual stores: order load, network loss, OTA rings, nightly reconciliation |
| Hardware | manual, by matrix | — | Printer models, payment terminals, sudden power loss |

Only the last layer needs a human. **Property-based tests target the data-correctness laws** in [pos-spec.md](pos-spec.md) §14 — business date, line snapshots, split rounding, exclusive settlement — because those are the invariants where a subtle break costs money.

There is deliberately **no repository-wide coverage gate**: percentage targets are easy to game, especially by generated tests. The real safety net is contract tests plus the simulator. The single exception, enabled once more than three people work here, is that `pos-core` coverage must not decrease.

## 8. Architecture decision records

`docs/adr/NNNN-title.md`, one page each: context → options considered → decision → consequences accepted. Write the ADR **before** the code for anything listed in [AGENTS.md](../AGENTS.md) §7.

Recorded so far:

| ADR | Decision |
|---|---|
| 0001 | Offline-first at the store; the cloud is never in the sales path |
| 0002 | One binary per tier (modular monolith) |
| 0003 | Cattle, not pets: activation codes plus a single-active lease |
| 0004 | All configuration lives in the cloud |
| 0005 | Country-neutral core; fiscalisation as a country module |
| 0006 | Own the boundaries: ports and adapters |
| 0007 | In-house implementation strategy (architecture.md Appendix C) |
| 0008 | PostgreSQL partitioning instead of one database per store |
| 0009 | **Licence: proprietary, internal use.** Technical discipline is unchanged — the repository stays free of copyleft dependencies so opening the source later remains possible without untangling anything. |
| 0010 | snake_case everywhere; the four deliberate deviations from Google AIP |
| 0011 | Country lives in the hostname; redirect, never proxy |
| 0012 | QR ordering is a cloud module reusing the `OrderIn` port |
| 0013 | Async strategy across the boundary: sans-I/O domain core, async ports |
| 0014 | Date, time, and timezone library for business-date derivation |
| 0021 | Corrected sixteen-port list, superseding 0006 |
| 0024 | `PROTOCOL_VERSION` negotiation on the wire |
| 0026 | Port shapes: the shared failure type, the transaction handle, and three corrections to 0013 |
| 0027 | Country modules: a bundle at `countries/<cc>/`, selected by Cargo feature |
| 0025 | Receipt-number authority is configuration; gapless only while one store authority is reachable |
| 0028 | Settlement and payment invariant: tendered vs applied, tips separate, explicit rounding, tax per class |
| 0029 | Append-command merge: terminal states win, other fields last-writer-wins on (event_time, device_id) |
| 0015 | SQLite access at the edge: `rusqlite` behind a dedicated single-writer thread |
| 0017 | Migrations: forward-only, additive, enforced by a `cargo xtask migrations` gate |
| 0018 | Edge HTTP/WebSocket stack: axum + tower, a `broadcast` fan-out under 50 ms, the UI embedded with `rust-embed` |

The full index, including records reserved for decisions not yet taken, is in [`docs/adr/README.md`](adr/README.md).

## 9. Code and documentation rules

Design principles (SOLID, KISS, DRY, YAGNI) are stated as concrete, checkable rules in [`design-principles.md`](design-principles.md); the laws below are the mechanical subset that CI enforces.

Lint configuration is **code**, committed to the repository (`[workspace.lints]`, `rustfmt.toml`, `clippy.toml`), with strictness graded by layer:

| Layer | Enforced |
|---|---|
| `pos-core`, `pos-ports`, `pos-proto` | `forbid(unsafe_code)`, no `unwrap`/`expect`/`panic`, `deny(missing_docs)`, no `println!` (use `tracing`), dependency allowlist |
| Adapters | `unsafe` permitted for FFI but every block needs a `// SAFETY:` comment (lint-enforced); no `unwrap` on the main path; docs warn-level |
| Binaries and tests | `unwrap` allowed in tests; binaries use `anyhow` at the outermost layer |

The seven standing code rules — integer money, clock and id through ports, no blocking in async, bounded channels, events only via `TxContext`, no hardcoded user-facing strings, and naming compliance — are listed with rationale in [AGENTS.md](../AGENTS.md) §2.

**Error policy:** library crates return concrete error enums (`thiserror`), never `String`; significant results are `#[must_use]`.
**Logging:** structured `tracing` with spans; **PII is never logged**, because logs travel to the cloud.
**API stability:** the three backbone crates keep a public-API snapshot in the repository. Changing the API means updating the snapshot **in the same pull request**, which makes every change visible and reviewable. Deprecate for at least two releases before removal; the MSRV is stated and only rises in a minor version.

**Documentation has four tiers, each with one rule:** rustdoc is mandatory on public items of the backbone crates and doc examples must compile · every crate has a README following a four-part template (what it does, where it sits, how to test it, who owns it) · guides in `docs/guides/` are written for people *using* the framework (getting started, writing an adapter, adding a country module, running the simulator) · ADRs explain *why*.

The rule that ties them together: **a behaviour change updates its documentation in the same pull request**, enforced by a checkbox reviewers verify.

**Language:** code, rustdoc, and commit messages in English; store-facing runbooks may be translated for the local team.

## 10. Working with AI contributors

AI agents follow the same rules as humans, with three additional guardrails matched to three specific risks:

| Risk | Guardrail |
|---|---|
| Licence contamination (a model reproducing code of unknown origin) | Review rule: no unexplained large blocks without provenance. `cargo-deny` cannot catch this — humans must. |
| Subtle bugs that are syntactically correct and semantically wrong | The net is contract tests, the fleet simulator, and nightly reconciliation — not reading diffs |
| Prompt injection through issues or third-party content | Agents run without secrets, hold least privilege, and cannot merge. The release path is immune because signing keys are offline. |

Pull requests with AI assistance carry the `ai-assisted` label — for traceability, not for a lower bar. A human always merges.

## 11. Fork-and-deploy

### 11.1 Flow

```
Fork → set Secrets → Actions → "Deploy" → Run
   │
   ▼
GitHub Action:
  1. builds the pos_cloud image and compresses it
  2. connects over SSH using the repository secrets
  3. runs bootstrap.sh (idempotent):
     • installs Docker if absent
     • GENERATES operational secrets ON THE SERVER (database password, NATS seed, S3 keys)
       → stored in /opt/pos/secrets, never sent back to GitHub
     • docker compose up: pos_cloud, PostgreSQL, NATS, Garage
       (optional "monitoring" profile: VictoriaMetrics, Grafana)
     • Caddy obtains and renews TLS certificates
  4. prints the URL and a ONE-TIME setup token (24-hour expiry)
   │
   ▼
open https://admin.<domain>/setup?token=…  → create the super-admin (TOTP required)
   → /setup permanently locks itself
   → create Tenant → subdomain live immediately → invite the tenant admin by link
   → Brand → Store (sample menu + readiness checklist) → Export installer + activation code
```

### 11.2 Secrets

| Secret | Required | Purpose |
|---|---|---|
| `VPS_HOST`, `VPS_SSH_PORT`, `VPS_USER`, `VPS_SSH_KEY` | yes | Server access (a key created solely for deployment) |
| `DOMAIN`, `ACME_EMAIL` | recommended | Hostname and certificate notifications |
| `CF_DNS_API_TOKEN` | required for multi-tenant subdomains | ACME DNS-01: closes port 80 and issues a wildcard certificate |
| `RCLONE_REMOTE_*` | optional | Second-tier backup target |

**Two-tier secret principle.** GitHub holds only the *key to the door* — SSH access and public information. **Operational** secrets are generated on the server at first boot and stay there. This means repository administrators do not automatically hold the production database password, rotating operational secrets requires no GitHub change, and forking or sharing the repository can never leak system secrets. The user experience is unchanged: four to six secrets and one button.

**Trust boundary of the setup token:** it is printed in the Actions summary, so anyone with *read* access to the repository can see it until it is used or expires. With a private repository and few collaborators this is acceptable — it is one-time and `/setup` locks itself permanently — but be explicit: **read access to the repository equals first-time setup rights.** Do not make the repository public before setup.

### 11.3 Image delivery

The Action builds the image, saves and compresses it, ships it over the **existing SSH channel**, and loads it on the server. A registry (ghcr.io) was rejected for now: a private repository implies a private registry, which means the server must hold a pull token — one more secret to rotate and one more third party in the deployment path. Save-and-load costs 30–60 seconds per deploy and needs **no new secret and no new dependency**. Switch to a registry when there are multiple servers or cells, and record it as an ADR.

Docker is the one infrastructure dependency the architecture accepts (100–200 MB), because it makes `bootstrap.sh` behave identically on every Ubuntu or Debian host — the precondition for "deploy and it just works" for people who are not us. The minimal stack is four containers, roughly 1.2–1.5 GB of RAM.

### 11.4 TLS and DNS

Caddy handles Let's Encrypt automatically; later this moves inside `pos_cloud` via `rustls-acme` and Caddy disappears.

When DNS is hosted at Cloudflare, three rules apply:

1. **Keep every record grey (DNS only).** Proxying means Cloudflare terminates TLS and can read all traffic — the same concern that led us to defer tunnels. If it is ever enabled, the SSL mode must be *Full (Strict)*.
2. **Never use the "Flexible" SSL mode**: the browser sees a padlock while the Cloudflare-to-server leg runs plain HTTP across the internet. That is worse than no TLS, because it looks safe.
3. **Prefer ACME DNS-01** with a scoped API token: it closes port 80 and issues a wildcard certificate, which is what makes per-tenant subdomains instant.

Everything needed here — DNS hosting, wildcard records, the API token — is inside Cloudflare's free tier; certificates come from Let's Encrypt at no cost.

### 11.5 Multi-tenant subdomains and the super-admin

Two administrative tiers: the **platform super-admin** (created once through `/setup`, TOTP mandatory, lives at `admin.<domain>`, manages tenants and fleet-wide settings) and **tenant admins** (invited by link, live at `<slug>.<domain>`, full control inside their tenant).

Mechanics: one wildcard DNS record, one wildcard certificate (so creating a tenant needs no certificate issuance), host-header lookup to resolve the tenant, and row-level security for isolation. Slugs are `a-z0-9-`, three to thirty characters, with reserved names blocked. **Session cookies are scoped to each subdomain** and never to the parent domain — otherwise one tenant's session would be sent to another, which is the most serious isolation failure in a multi-tenant system.

Recovery paths: a lost setup token is replaced by re-running the workflow (bootstrap is idempotent); a super-admin who loses both password and TOTP is recovered through a workflow input `reset_admin=true` that prints a one-time recovery token — the only break-glass path, and it sits behind GitHub Environment permissions.

Invitations do not require SMTP: the system generates expiring links that an admin sends over any channel.

### 11.6 Upgrade, rollback, and other lanes

Upgrading is re-running the workflow with a newer tag; rolling back is re-running it with an older one. Both directions have zero manual steps on the server. Stores are unaffected either way because they are autonomous.

Kubernetes/GKE is supported through `deploy/k8s/` with the same container images, but it is a **paid** lane that conflicts with the near-zero cost target — and its default persistent disks are network storage, so local SSD must be selected explicitly. The recommended default remains a single VPS.

## 12. Governance and risk

| Risk | Control |
|---|---|
| GitHub account loss | Daily mirror to a second remote |
| GitHub Actions supply chain | **Pin every action to a commit SHA**, not a tag; minimise third-party actions |
| Bus factor of one | At least two maintainers with release rights and access to the spare signing key; every procedure lives in a runbook, not in someone's head |
| crates.io outage or a deleted crate | Vendored dependencies |
| Secret leakage | Secret scanning, push protection, and signing keys that never touch CI |
| Low-quality AI contributions | §10 |

Two more files live at the repository root: `MAINTAINERS.md` (who holds which rights) and `SECURITY.md` (how to report a vulnerability).

**Process scales with the team.** Machine-enforced rules are on from the first commit because they cost nothing to run. Human-operated process — full RFCs, review SLAs, the `pos-core` coverage gate — is written down now and **activated when more than three people work here regularly**. One exception is active immediately: an ADR before code for anything touching ports or the protocol.

## 12b. Keeping documentation current

Documentation lives in the repository and moves with the code that it describes.

- **Same-PR rule.** A change in behaviour and its documentation land together. The pull-request template asks explicitly; CI flags pull requests touching `crates/pos-core`, `crates/pos-proto` or the HTTP surface with no change under `docs/`.
- **Header on every document:** `Status` (draft / accepted / superseded), `Owner`, `Last reviewed`. A document unreviewed for two releases is raised in the release checklist.
- **Generated, not transcribed.** OpenAPI, the permission matrix, the event catalogue and the design-token table are produced from code. Hand-copying them into prose is prohibited, because copies rot.
- **Automated checks:** internal link checker; snapshot drift (API, events, permissions); generated-OpenAPI diff; a scan for `TODO` markers older than one release.
- **Three artifacts, three jobs.** `docs/**` = how it is now · `docs/adr/**` = why we decided · `CHANGELOG.md` = what changed. Keep each in its lane.
- **Languages.** Code, rustdoc, commits, specifications: English. Store-operator runbooks and release notes for restaurant staff: local language.

## 12c. Change history and release notes

**Every version is traceable by anyone, without reading diffs.**

- `CHANGELOG.md` follows Keep a Changelog (Added / Changed / Deprecated / Removed / Fixed / Security). Entries are generated from Conventional Commits, then curated: one line per user-visible change, written for the reader.
- **Every tag ships release notes** containing: Highlights · **Upgrade notes** (database migrations, `PROTOCOL_VERSION` change, new or changed default configuration, required manual steps) · Breaking changes · Fixed · Known issues · the artifact hashes and signature.
- **Mandatory entries:** anything touching the wire protocol, a migration, a permission identifier, a default configuration value, or fiscal behaviour.
- **Hotfixes** cut from `release/x.y` carry their own notes and are cherry-picked back to `main` in the same pull request that documents them.
- **Two audiences.** Engineering notes in English in the release; a short operator summary in the local language for store managers, describing what they will see on screen.
- **Deprecation is announced, not sprung:** a public API field, event, or permission is marked deprecated in the release notes for at least two releases before removal, and removal is itself a headline entry.

## 13. Build order

| Phase | Work | Outcome |
|---|---|---|
| **Week 0–1** (before product code) | Workspace skeleton with the correct boundaries and the dependency-rule test · pinned toolchain · `justfile` · `deny.toml` · PR pipeline · branch protection and CODEOWNERS · `AGENTS.md`, `CONTRIBUTING.md`, templates · two signing keypairs stored per architecture.md §4 · repository mirror · ADRs 0001–0009 | A repository where the rules enforce themselves |
| **First month** | `pos-core` state machine and the four data-correctness laws first · contract tests for `EventStore`, `MessageLink`, `PrinterDriver` · `examples/minimal-edge` · first guides · `MAINTAINERS.md`, `SECURITY.md` | An outsider — human or AI — can write an adapter using only what is in the repository |
| **Before 50 stores** | Fleet simulator · automated fork-to-UI end-to-end test · adapter template (extracted when writing the *third* adapter, not before) | Load and rollout risks are provable without risking real stores |
| **When the team exceeds three** | RFC process · review SLA · `pos-core` coverage gate | Heavier process arrives exactly when it starts paying for itself |

Nothing in phase one produces product code, yet skipping it means retrofitting every rule into code that already exists.
