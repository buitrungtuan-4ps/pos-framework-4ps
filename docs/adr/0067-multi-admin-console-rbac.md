# ADR-0067 — Multi-admin console identities with role-based access

**Status** Accepted · **Owner** @maintainers-security · **Last reviewed** 2026-08-26
**Supersedes** [ADR-0034](0034-super-admin-auth.md) · **Relates to** [ADR-0030](0030-pairing-and-offline-auth.md) · [ADR-0037](0037-api-keys.md) · [ADR-0045](0045-first-boot-admin-enrolment.md) · `docs/pos-spec.md` §9–§10 · `docs/cloud-admin-ux-plan.md` (Track G1)

**Context.** [ADR-0034](0034-super-admin-auth.md) gave the cloud one **single** super-admin
(Argon2id password + mandatory TOTP + a host-only `__Host-` session). That was right for first boot
and a solo operator, but a back office is run by a *team*: a shared credential gives no per-person
accountability, no least privilege (everyone who can read a rollup can also mint API keys and edit
every tenant), and no way to off-board one person without rotating the secret for all. G1 introduces
**multiple named console admins with roles**, while keeping — unchanged and per-user — every crypto
and session property ADR-0034 established (they were the hard security decisions; this ADR reuses
them, it does not revisit them).

**Decision.**

- **`admin_users` replaces the single-row `super_admin`.** Each admin is a row: a ULID id, a unique
  `email` (the login identity), a display `name`, a `role`, a `status` (`active`/`suspended`), and
  **its own** Argon2id password hash, TOTP secret, and last-used TOTP step. The migration converts
  the existing `super_admin` row into one `owner` admin_user, so an installation upgrades in place and
  the ADR-0045 `reset_admin` break-glass still re-seeds that owner. There is no longer a global shared
  credential; there is always at least one `owner`.

- **Four console roles, least-privilege, over the §9 permission registry pattern.** `owner` (all,
  including managing admin_users), `admin` (all tenant data; cannot manage admin_users), `ops`
  (day-to-day: devices, activation, webhooks, config publish; no API-key or tenant/brand creation),
  `viewer` (read-only). Every `/admin` route declares the permission it needs and the session guard
  enforces it; the role→permission templates are built with the **same compile-forced registry
  pattern as `pos-core` §9**, so adding a permission fails to compile until every role template
  accounts for it — deny by default. Per-tenant scoping (an admin restricted to a subset of tenants)
  reuses the registry relation; `owner`/`admin` default to all tenants.

- **Invitation, never admin-set passwords.** An `owner`/`admin` invites by email; the server mints a
  single-use, TTL-bounded invite token (only its hash stored) that the invitee exchanges to set their
  **own** password and enrol TOTP — reusing ADR-0034's Argon2id + RFC-6238-SHA1 + provisioning-QR path
  verbatim. No admin ever learns or sets another's password.

- **Accountable, revocable sessions.** `admin_sessions` gains `admin_id`, client IP, and user-agent;
  an admin can list their live sessions and revoke one or all-others; sessions keep a sliding TTL with
  an idle timeout. The `__Host-` host-only cookie, the SHA-256-only token storage, the two-factor
  check, the no-which-factor-failed oracle, and single-use TOTP from ADR-0034 are **unchanged**.
  Concretely (slice 4): the former fixed session TTL (`admin_session_ttl_secs`, 8h) becomes the
  **absolute cap** a session can never live past; a new `admin_session_idle_ttl_secs` (default 30 min)
  is the **idle window**, and a real guarded request slides `expires_at = min(now + idle_ttl,
  absolute_cap)` atomically as the guard reads the session. The lightweight liveness poll
  (`/admin/session`) deliberately does **not** slide, so an admin who stops acting still times out even
  with the console tab open. Session management is **self-service** — every authenticated admin manages
  their own sessions regardless of role, and every listing/revocation is scoped to the caller — so it
  is gated by the session guard, not a `console.*` permission. The revocation handle is the session's
  token hash (never the token, not reversible to it), and behind the P8 reverse proxy the client IP is
  read from `X-Forwarded-For`; the IP/user-agent are shown only to the admin whose session it is and
  are never trusted for authorization. Sessions minted before the additive migration keep their
  original fixed expiry (a `NULL` cap/window means "do not slide").

- **Defence-in-depth ADR-0034 deferred:** a login rate-limit (per email and per IP, sliding window)
  and `/admin` security headers (`Content-Security-Policy`, `X-Content-Type-Options: nosniff`,
  `Referrer-Policy`, `X-Frame-Options: DENY`) now land here. TOTP **re-enrolment** (a signed-in admin
  rotates their own secret) and one-time **recovery codes** (hashed, for a lost authenticator) close
  the account-recovery gap without the break-glass.
  Concretely (slice 5): the rate-limit is a sliding window (a per-key deque of recent attempt
  instants), default 10 attempts / 5 minutes, checked *before* the Argon2id verify so it costs an
  attacker a `429` rather than the server a hashing storm and can never leak whether the credential
  was right; a refused attempt is not recorded, so it does not extend a legitimate admin's window.
  It is keyed by client IP today (read from `X-Forwarded-For` behind the P8 proxy); the `email:…` key
  the "per email" limit needs lights up when per-admin email login lands — the limiter already
  refuses if *any* presented key is over its limit. State is in-process (single box) and ephemeral,
  so a restart fails open, never locking an operator out. The security headers are applied as one
  router layer over every response — the console SPA, its assets, and the `/admin`/`/v1` APIs alike —
  so the SPA served at the site root is protected too; the CSP locks `script-src` to `'self'` (the
  built SPA carries no inline script) and relaxes only `style-src` with `'unsafe-inline'`, a
  deliberately bounded concession for runtime inline styles.
  Concretely (slice 6): because sign-in is still the single super-admin credential (slice 2b kept it
  as-is; per-admin email login is a later slice), re-enrolment and recovery act on **that** credential
  — the one login verifies — and the recovery codes belong to the owner. `POST /admin/totp` rotates
  the TOTP secret only after re-confirming the current *password*, so a session-only attacker cannot
  re-enrol; requiring the current TOTP would be self-defeating for a lost authenticator, so the
  knowledge factor alone gates it. `POST /admin/recovery-codes` (re)generates ten codes, returned once
  and stored only as `SHA-256`; `/admin/login` accepts a `recovery_code` in place of the TOTP code,
  verifying the password first (a wrong password never burns a code) and consuming the code atomically
  single-use, with every failure collapsing to the same generic refusal as the TOTP path. Both
  management routes are self-service (session-gated, no `console.*` permission). This is complementary
  to — not a replacement for — the ADR-0045 `reset_admin` break-glass, which stays the last resort
  when both factors and all recovery codes are lost.

- **Console-only identity (Fork E).** These identities authenticate the **console only**. Store staff
  keep the edge offline-PIN system ([ADR-0030](0030-pairing-and-offline-auth.md)); there is no unified
  console+store identity. The two live in different trust domains with different lifecycles and
  offline needs — folding the edge's offline PIN into the cloud identity model would buy nothing now
  and couple two things that should stay independent.

**Rejected.**

- **Keep the single shared super-admin** — no accountability, no least privilege, no per-person
  off-boarding. The reason G1 exists.
- **One unified identity for console + store staff** (the other Fork E option) — different domains
  (online TOTP vs offline PIN, ADR-0030), different lifecycles, no current need; revisit only if a
  concrete cross-surface use case appears.
- **An admin sets another admin's password** — rejected; invitation + self-set only, so a password is
  never known to anyone but its owner.
- **A coarse "admin / not-admin" flag instead of roles** — rejected; least privilege needs real roles,
  and the §9 registry pattern makes them cheap and compile-safe.
- **Rolling our own anything** — unchanged from ADR-0034: Argon2id and RFC 6238 stay.

**Consequences.**

- One additive migration: `admin_users`, `admin_invites`, `admin_recovery_codes`; `admin_sessions`
  gains `admin_id`/`ip`/`user_agent`; the `super_admin` row migrates to an `owner` `admin_user`. No
  destructive change; `reset_admin` still works.
- The `/admin` session guard becomes role-aware; each route names a required permission. A new
  `AdminStore` seam surface (users, invites, recovery codes, session listing/revocation) with a
  store-postgres impl and an in-memory fake, contract-tested like the others.
- ADR-0034 is superseded but retained: it remains the record of the single-admin era and of the
  crypto/session decisions G1 inherits unchanged.
- **Security-reviewed before merge**, and landed in reviewable slices: (1) schema + `AdminStore`
  seam + fake; (2) role/permission registry + role-aware guard; (3) invitation + self-enrol; (4)
  session listing/revocation + sliding/idle TTL; (5) rate-limit + security headers; (6) TOTP
  re-enrol + recovery codes; (7) the console UI (admins list, invite, my-sessions, my-security).
