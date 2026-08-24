# ADR-0045 — First-boot super-admin enrolment, and the reset break-glass

**Status** Accepted · **Owner** @maintainers-security · **Last reviewed** 2026-08-20
**Relates to** [ADR-0034](0034-super-admin-auth.md) · [ADR-0044](0044-fork-and-deploy.md) · `docs/roadmap.md` P8

**Context.** [ADR-0034](0034-super-admin-auth.md) built the super-admin *login* — Argon2id password,
mandatory TOTP, a server-side session — and shipped the `super_admin` table it lives in, but left one
thing to "P8 bootstrap": **how the first credential gets there**. Today `AdminStore` can load a
credential and never write one, so a freshly-deployed cloud has a login page and no way past it. The
P8 exit criterion is precisely "admin UI live **with a one-time setup token**": a fork, once deployed,
must let exactly one person enrol themselves as the super-admin — choosing a password and receiving a
TOTP secret — over the network, with **no `psql` on the server**. And when that person loses their
authenticator, there must be a break-glass that a second human approves.

The constraint that shapes everything: the server must **never learn a plaintext password from
configuration**, and the setup path must not be a standing "create an admin" hole. So seeding a full
credential from an environment variable is out — the operator picks the password interactively; the
box only holds a *token* that authorises the one enrolment.

**Decision.**

- **A token-gated `POST /admin/setup`, self-disabling once an admin exists.** `bootstrap.sh` (P8b)
  mints a 256-bit `admin_setup_token` into the box-local `cloud.toml` (mode `600`, the same trust
  boundary that already holds the database password) and prints it once. The route accepts a chosen
  password and the token, compares the token in **constant time**, and — only if no admin is
  provisioned yet — generates a fresh 32-byte TOTP secret, hashes the password with Argon2id under a
  CSPRNG salt, writes the single credential, and returns the **TOTP enrolment** (the `otpauth://` URI
  and its base32 secret) exactly once. A second call returns `409`, because provisioning is
  `INSERT … ON CONFLICT (id) DO NOTHING`: the row is the single-row `super_admin` table, so the first
  enrolment wins and the token is thereafter inert. With no token configured the route is `404` —
  off. This adds one method to `AdminStore` (`provision_credential`) and no new migration: the table
  from [ADR-0034](0034-super-admin-auth.md) already has the shape.

- **The TOTP secret is generated on the server and shown once; the password never is.** The operator
  supplies the password (over Caddy's TLS, [ADR-0044](0044-fork-and-deploy.md)); the server hashes it
  and forgets it. The server generates the TOTP secret because a shared secret has to originate
  somewhere and the client cannot be trusted to pick entropy — it is returned once, in the enrolment
  response, for the operator's authenticator app, and never again (the store keeps only the raw
  secret for verification, never re-emits it). A minimum password length is enforced (`422` below it)
  so first-boot cannot mint a trivially weak super-admin.

- **base32 and the `otpauth://` URI are hand-rolled, not a new dependency.** RFC 4648 base32 (upper,
  unpadded) is ~20 lines and the URI is a `format!`; pulling a crate for either would be a new
  `cargo-deny` entry for no real saving ([ADR-0007](0007-in-house-vs-dependency.md)). The URI fixes
  `algorithm=SHA256&digits=6&period=30` to match the verifier ([ADR-0034](0034-super-admin-auth.md)),
  and the issuer/account carry no characters needing percent-encoding.

- **Reset is a DB break-glass in the deploy workflow, behind a GitHub Environment — not app code.**
  Losing the authenticator is recovered by re-running the deploy workflow with `reset_admin=true`,
  which is gated by the `production` Environment's required reviewer (a second human). It runs one
  idempotent statement on the box — `DELETE FROM super_admin; DELETE FROM admin_sessions;` — which
  removes the credential **and** every live session, then the operator re-enrols through
  `/admin/setup`. Keeping reset in the ops layer means no `reset_admin` flag rides in the app's
  container environment, where it would silently wipe the admin on every restart.

**Rejected.**

- **Seeding a full credential from `bootstrap.sh` / an env var** — rejected: it would put a plaintext
  password (or a pre-hashed one the operator never chose) into config, and the server would learn a
  secret it must never hold. The operator chooses the password interactively; the box holds only the
  authorising token.
- **A standing, always-open enrolment route** — rejected: it self-disables the moment an admin exists
  (`409`) and is off entirely without a configured token (`404`), so it is not a permanent
  admin-creation hole.
- **Storing the setup token hashed in config** — rejected as false economy: `cloud.toml` is already a
  `600` file holding the database password, so a hash buys nothing an attacker who can read the file
  has not already won; the constant-time compare is what defends against a *remote* guesser.
- **A `reset_admin` env var read at boot** — rejected: a value set in the compose environment persists
  across restarts and would wipe the admin every boot. Reset is a one-shot, so it lives in the
  workflow as a one-shot.

**Consequences.**

- `AdminStore` gains `provision_credential`; `store-postgres` gains the `INSERT … ON CONFLICT DO
  NOTHING`; `pos-cloud` gains `crate::auth::enrol` (base32, the URI, the constant-time compare),
  the `/admin/setup` route, and an `admin_setup_token` config field. No new migration; no new crate.
- The enrolment response carries the TOTP secret, so it is sensitive: it is returned once, over TLS,
  to the token-bearing caller, and `Debug` on the request redacts the password (as login already
  does). It is an `/admin` route, so it is deliberately absent from the public `/v1` OpenAPI.
- The break-glass is only as strong as the `production` Environment's protection: the fork must
  configure it with a required reviewer, or `reset_admin` is a single-actor action. The deploy
  runbook (P8e) records this.
