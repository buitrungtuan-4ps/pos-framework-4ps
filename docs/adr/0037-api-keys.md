# ADR-0037 — Scoped per-tenant API keys: hashed bearer tokens, tenant-bound, deny-by-default

**Status** Accepted · **Owner** @maintainers-security · **Last reviewed** 2026-08-20
**Relates to** [ADR-0019](0019-openapi-generation.md) · [ADR-0026](0026-port-shapes.md) · [ADR-0034](0034-super-admin-auth.md) · `docs/roadmap.md` P7

**Context.** The public `/v1` API ([ADR-0019](0019-openapi-generation.md)) is built against by machine
integrators, not humans, so the interactive super-admin sign-in ([ADR-0034](0034-super-admin-auth.md))
does not fit: there is no one to type a password and a TOTP code. Programmatic callers need a bearer
credential — an API key. In a multi-tenant system the key is also where the isolation boundary lives:
a key must be able to reach one tenant's data and nothing else, and it must carry no more privilege
than the integration needs.

**Decision.**

- **A key is `pos_<id>_<secret>`; only a hash of the secret is stored.** The `id` is public and looks
  the record up; the `secret` is the bearer proof. The cloud stores `SHA-256(secret)` and never the
  secret, so a database leak yields nothing usable, and verification is a constant-time compare of the
  hash. **SHA-256, not Argon2** — deliberately: the secret is a long CSPRNG token, not a low-entropy
  human password, so there is no dictionary to slow (Argon2's whole purpose), and a per-request fast
  hash is what an API needs. The `pos_` prefix is fixed and recognisable so a leaked key trips secret
  scanners — including this repo's own `secrets` job.

- **Every key is bound to one tenant.** The stored record carries a `tenant_id`, and verification
  returns a `Grant` whose `tenant()` a handler must check against the resource it is serving. This is
  the isolation control for `/v1`: a key cannot be used to read or change another tenant's data,
  independent of what the handler's SQL would otherwise allow (RLS is the second layer).

- **Deny by default, by scope.** A key holds a set of `Scope`s and authorises nothing outside it
  (`Grant::authorizes`). A read-only integration gets a read scope and cannot write; the scope set
  grows with the API rather than every key being all-powerful.

- **Revocable, optionally expiring, shown once.** A key can be revoked (immediate refusal) and given
  an expiry; `issue` returns the full token exactly once and keeps only the hash, so a lost key is
  re-issued, never recovered. As with the super-admin, the specific rejection reason (malformed /
  unknown / bad secret / revoked / expired) is for the server's log; the client is told only that the
  key was refused, so it is not an enumeration oracle.

**Rejected.**

- **Argon2 (or any slow KDF) for the secret** — rejected: it buys nothing for a high-entropy random
  token and would put a deliberately-expensive hash on every API request. Slow hashing is for
  guessable human passwords ([ADR-0034](0034-super-admin-auth.md)); this is the opposite case.
- **Storing the secret reversibly** (encrypted-at-rest but recoverable) — rejected: a hash means a
  store compromise cannot yield working keys, and there is never a reason to read a key back.
- **Unscoped or cross-tenant keys** — rejected outright: an unscoped key is a standing
  privilege-escalation, and a cross-tenant key is the multi-tenant isolation failure the whole design
  exists to prevent.
- **Reusing the super-admin session mechanism** for machines — rejected: cookies, TOTP, and a
  browser-oriented session make no sense for a server-to-server caller; the two callers get the two
  mechanisms that fit them, both living in `auth`.

**Consequences.**

- No new dependencies: hashing reuses the `sha2` already in `pos-cloud`.
- The engine — token parse, `issue`, constant-time `verify`, and the tenant/scope `Grant` — is pure
  and unit-tested with no store: round-trip issue→present→verify, wrong secret, id mismatch,
  revoked, expiry (inclusive), malformed tokens, deny-by-default scoping, and that neither a
  presented nor a stored key leaks its secret through `Debug`. Test fixtures use an obviously-fake,
  low-entropy secret so no real key material is committed.
- **Deliberately not here yet:** the `store-postgres` table that persists keys and the lookup by id,
  the key-provisioning admin route (which generates the CSPRNG id + secret and returns the one-time
  token), and the `/v1` extractor that pulls the bearer token, verifies it, and enforces
  `tenant()` + the required `Scope` on each route. Those are the wiring; this slice is the credential
  and its checks.
