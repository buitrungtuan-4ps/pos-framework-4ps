# ADR-0034 — Super-admin auth: Argon2id + mandatory TOTP, host-only sessions

**Status** Accepted · **Owner** @maintainers-security · **Last reviewed** 2026-08-20
**Relates to** [ADR-0004](0004-cloud-owned-configuration.md) · [ADR-0030](0030-pairing-and-offline-auth.md) · [ADR-0031](0031-cloud-adapter-transports.md) · `docs/pos-spec.md` §10 · `docs/roadmap.md` P7

**Context.** The super-admin is the most privileged identity in the cloud: it manages every tenant's
configuration, keys, and fleet. Its sign-in is the highest-value target in the system, so it needs
more than a password, and the pieces around it have sharp, well-known failure modes: a password-only
login, a weak password hash, a second factor that can be replayed, an oracle that reveals which
factor failed, and — the one `docs/roadmap.md` singles out as the worst multi-tenant isolation
failure — a session cookie scoped to the parent domain, which then travels to every tenant's
subdomain.

**Decision.**

- **Argon2id for the password.** The stored credential is an Argon2id PHC string at `argon2`'s
  default cost — the *same* primitive and crate the edge uses for offline PIN hashes
  ([ADR-0030](0030-pairing-and-offline-auth.md)). Only the hash is ever stored; the password is never
  logged, spanned, or persisted. A malformed stored hash verifies nothing (returns `false`, not an
  error), so a corrupted credential cannot become a way in.

- **A mandatory TOTP second factor (RFC 6238), over HMAC-SHA1.** There is no password-only path:
  [`authenticate`](../../crates/pos-cloud/src/auth/mod.rs) succeeds only when the password verifies
  *and* a TOTP code verifies. TOTP runs over **HMAC-SHA1** — the RFC 6238 default and, decisively, the
  only algorithm the authenticator apps operators actually use compute: Google Authenticator and
  Microsoft Authenticator **ignore** the `otpauth://` URI's `algorithm` field and always assume SHA1,
  so a SHA256 secret produced codes that never verified and sign-in failed immediately after
  enrolment (see the amendment below). The provisioning QR sets `algorithm=SHA1` explicitly anyway, so
  the URI-honouring apps agree with the ones that assume it. Codes are 6-digit on a 30-second step,
  accepted within a **±1-step skew window** for clock drift, and **single-use**: verification returns
  the step it matched and refuses any step at or below the last one accepted, so a code captured
  within its validity window cannot be replayed.

  > **Amendment (2026-08-25).** This decision originally chose **HMAC-SHA256**, to avoid pulling a
  > second `sha1` crate version (`sha1` 0.10 is already in the tree via axum's WebSocket handshake, on
  > the older `digest` 0.10 line that `hmac` 0.13 cannot reuse). That was dependency hygiene deciding
  > what looked like a free choice — but it was not free: it made first sign-in **impossible** with the
  > authenticator apps operators actually run, because those apps ignore `algorithm=SHA256`. Switched
  > to HMAC-SHA1; the duplicate `sha1` line (0.11 on the `digest` 0.11 line, alongside axum's 0.10) is
  > the accepted cost, carried by a documented `bans.skip` in `deny.toml`. HMAC-SHA1 is sound for TOTP
  > (the security is the shared secret's entropy and the single-use/skew rules, not the hash). Existing
  > enrolments must re-enrol via the `reset_admin` break-glass path (ADR-0045); there is at most one.

- **Both factors evaluated, one generic failure to the client.** `authenticate` computes the password
  and TOTP checks before returning any verdict, and the HTTP layer returns a single generic failure
  whichever was wrong. The specific reason (`BadPassword` / `BadTotp` / `TotpReused`) exists only for
  the server's log, so a prober cannot learn which factor it got right.

- **The session cookie is host-only, so it cannot cross subdomains.** The cookie is named with the
  `__Host-` prefix and set `Secure; HttpOnly; SameSite=Strict; Path=/` with **no `Domain`
  attribute** — host-only, and the `__Host-` prefix has the browser *enforce* that (it refuses a
  `__Host-` cookie carrying a `Domain` or lacking `Secure`/`Path=/`). An admin session for one
  tenant's host is therefore never sent to another's, backed by the browser rather than by our
  discipline.

**Rejected.**

- **Password-only, or optional/second-factor-on-request TOTP** — rejected: this identity is too
  privileged for one factor, so TOTP is not a setting that can be off.
- **HMAC-SHA256 TOTP** — tried first and reverted (see the amendment above). It is spec-compliant and
  avoided a duplicate `sha1` crate, but Google/Microsoft Authenticator ignore `algorithm=SHA256` and
  compute SHA1 regardless, so real operators could never sign in. Dependency hygiene is not worth an
  unusable second factor; HMAC-SHA1 is the choice, and the duplicate `sha1` line is skipped in
  `deny.toml`.
- **A `Domain`-scoped session cookie** — rejected outright: it is the cross-tenant session leak the
  roadmap names as the worst isolation failure.
- **Revealing which factor failed** — rejected: it is a free enumeration oracle.
- **Rolling our own password hash or TOTP** — rejected: Argon2id and RFC 6238 are exactly the
  standard, vetted primitives [ADR-0007](0007-in-house-vs-dependency.md) says to buy.

**Consequences.**

- `pos-cloud` gains `argon2` (already at the edge, pure). TOTP adds **no** crypto crate — it reuses
  the `sha2` + `hmac` the webhook signer already brought in — so no new dependency version and no
  `cargo-deny` change.
- The auth core is pure and deterministic: password verify/hash (salt injected), TOTP (time
  injected), the two-factor flow, and the cookie policy are all unit-tested with no clock and no
  network — against RFC 6238's published SHA256 vectors, the redaction of both secrets from `Debug`,
  the mandatory-second-factor and no-oracle rules, replay refusal, and the host-only cookie
  attributes. The three sources of randomness — the Argon2 salt, the TOTP secret, and the session
  token — are generated at the binary edge (a CSPRNG) and passed in, which is what keeps the core
  testable.
- **Landed since:** the credential and last-used TOTP step now persist in `store-postgres` (migration
  `0003`, a single-row `super_admin` table plus a server-side `admin_sessions` table), and the login
  wiring is built — `POST /admin/login` runs the two-factor check and, on success, mints a 256-bit
  CSPRNG session token, stores only its `SHA-256`, and sets the host-only `__Host-` cookie;
  `POST /admin/logout` revokes the session and clears the cookie; `GET /admin/session` is the guard
  every other `/admin` route stands behind. The session TTL is configuration
  (`admin_session_ttl_secs`, default eight hours). The scoped per-tenant API keys that authorise
  machine callers landed earlier ([ADR-0037](0037-api-keys.md)).
- **Deliberately not here yet:** TOTP enrolment/QR provisioning and the first-boot seeding of the
  `super_admin` row (the bootstrap `reset_admin` break-glass, P8); the login route's rate-limit
  (defence-in-depth atop the already brute-force-resistant two factors); and a sweep of expired
  `admin_sessions` rows (until then the `expires_at` check already makes them unusable).
