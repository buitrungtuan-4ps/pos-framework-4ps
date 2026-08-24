# ADR-0060 — The cloud back-office is an embedded SolidJS SPA served by `pos_cloud` over the existing admin API

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-24
**Relates to** [ADR-0018](0018-http-websocket-stack.md) · [ADR-0002](0002-one-binary-per-tier.md) · [ADR-0020](0020-i18n-runtime.md) · [ADR-0033](0033-config-tree.md) · [ADR-0034](0034-super-admin-auth.md) · [ADR-0039](0039-config-delivery.md)

**Context.** Every back-office capability already exists in `pos_cloud` as a JSON endpoint under
`/admin` — super-admin login with mandatory TOTP ([ADR-0034](0034-super-admin-auth.md)), the
four-level config tree publish/read ([ADR-0033](0033-config-tree.md)), API-key provisioning, the
printer/KDS *discover→approve* queue, the translation grid, webhook registration, and activation
codes — plus the tenant-scoped `/v1` rollup read. What has never existed is a **screen**: an operator
drives all of it with `curl` or an API client today. `docs/roadmap.md` P7 deferred the dashboard as
"P6-family UI over the `/v1` read model", and P6 built the *edge* operator UI (SolidJS embedded in
`pos_edge` via `rust-embed`, [ADR-0018](0018-http-websocket-stack.md)) but not the cloud one.

The gap bites most on configuration delivery. The cloud is the authority for a store's config and the
store *pulls* it ([ADR-0039](0039-config-delivery.md)); there is no "export a file to the store". So
the only way to change a store's configuration is to `PUT /admin/stores/{id}/config/{level}` by hand.
A back-office screen over that endpoint is the missing half of the config-delivery story.

**Decision.**

- **The dashboard is a SolidJS + Tailwind single-page app in a new top-level `dashboard/`**, built by
  Vite to `dashboard/dist`, structured exactly like the edge `ui/` (design tokens, the ICU i18n
  runtime with `en` as the enforced floor and a `vi` catalogue, a typed `fetch` client, the
  `no-hardcoded-strings` lint). Reusing the edge's conventions keeps one visual and engineering
  language across both tiers rather than inventing a second.

- **`pos_cloud` embeds and serves it with `rust-embed`, exactly as `pos_edge` serves its UI.** A new
  `crate::assets` module embeds `../../dashboard/dist` (behind `debug-embed` so tests exercise the
  shipped path, and a `dev-ui` feature that reads the directory from disk for live development), and a
  `build.rs` writes a placeholder `index.html` when the directory is absent so a fresh checkout still
  compiles — the same mechanism `pos_edge` uses. The SPA is served as the router's **`fallback`**: the
  API routes (`/health`, `/internal/*`, `/v1/*`, `/admin/*`, `/sync/*`, `/activate`) match first, and
  everything else — `/`, client-routed paths, and the built static assets — resolves to the SPA, with
  an unknown path returning `index.html` for client-side routing.

- **Auth is the existing super-admin session cookie ([ADR-0034](0034-super-admin-auth.md)), unchanged.**
  The SPA holds no secret: it is static files, served publicly like any SPA, and every screen's data
  and every mutation go through the already-authenticated `/admin` endpoints. Login (password + TOTP)
  and first-run setup (the one-time token) are screens over `POST /admin/login` and `/admin/setup`; the
  session cookie the server already sets gates the rest, and `GET /admin/session` is the guard the shell
  consults on load. No new auth surface, no token in the page, no cookie on the parent domain.

- **One small new read endpoint: `GET /admin/stores/{store_id}/rollups/daily`.** The daily rollup is
  otherwise only readable at `/v1` under a tenant-scoped API key ([bearer, ADR-0037](0037-api-keys.md)),
  which the session-authed dashboard does not carry. Rather than make the dashboard mint and juggle a
  tenant key, this adds an **admin-session** read that names the tenant with a `?tenant=` query and
  reuses the same `RollupStore` read the `/v1` handler uses. This is the identical pattern the config
  routes already follow (`GET /admin/stores/{id}/config?tenant=…`): the super-admin is global by design
  ([ADR-0034](0034-super-admin-auth.md)) — it already authors and reads *every* tenant's configuration
  — so reading any tenant's rollup introduces no new trust boundary. It is a read; nothing is mutated.

- **The image is built the same way the edge image would be.** `deploy/Dockerfile` gains a Node build
  stage that runs `pnpm build` for `dashboard/` on the runner's native architecture (no emulation,
  consistent with the cross-compile in [ADR-0044](0044-fork-and-deploy.md)) and copies
  `dashboard/dist` into the Rust builder before `cargo build`, so the shipped binary embeds the real
  dashboard. A `dashboard` CI job typechecks, lints strings, and builds the SPA on every PR, mirroring
  the `ui` job; the CHANGELOG gate extends to `dashboard/`.

**Rejected.**

- **A separate `pos_dashboard` binary / a second deployed service** — rejected. It would need its own
  TLS, its own session story, and CORS to reach `/admin`; embedding it in `pos_cloud` keeps the
  "one binary per tier" shape ([ADR-0002](0002-one-binary-per-tier.md)) and means the dashboard is
  same-origin with the API it drives, so cookies and calls need no cross-origin handling.
- **Serving the SPA from Caddy as static files** — rejected. It would split the deploy artifact in two
  and lose the single-binary property; `rust-embed` keeps the cloud one file exactly as the edge is.
- **A `dyn`-free admin rollup read via a tenant API key held in the browser** — rejected. Putting a
  tenant key in the SPA is a secret in a public page and a second auth plane; the admin-session read is
  simpler and matches the existing admin-is-global config read.
- **Server-rendered HTML templates** — rejected. The team already owns the SolidJS + Tailwind + ICU
  toolchain from P6; a second rendering technology in the cloud is cost with no benefit.

**Consequences.**

- The config-delivery story is complete end to end: an operator publishes a store's configuration from
  a screen, and the store pulls it — no `curl`, no hand-built JSON.
- `pos_cloud` now serves static assets. This is the first non-JSON surface in the cloud; the router's
  fallback is the one place it lives, and the API routes are unaffected because they match first.
- A new contributor can operate a cell from a browser, which is the P13 "fork-to-UI" checklist's
  intent. The screens that need endpoints not yet built (a live sales chart beyond the daily rollup,
  bulk config diffs) are additive follow-ups over the same session.
- The dashboard reads and writes tenant configuration and tenant PII-adjacent data (buyer fields never
  appear here; the config tree and rollups do not carry raw PII). Nothing changes the retention or
  masking rules ([ADR-0035](0035-retention-and-pii-masking.md)); the dashboard shows only what the admin
  endpoints already return.
