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
- **Landed since:** the full wiring. The `store-postgres` `api_keys` table (migration `0002`) persists
  keys and answers the lookup-by-id; the `/v1` bearer extractor ([`auth::bearer`](../../crates/pos-cloud/src/auth/bearer.rs))
  pulls the token, verifies it, and enforces `tenant()` + the required `Scope` on each route; and the
  key-provisioning admin surface — `POST /admin/api-keys` (mints the CSPRNG id + secret, returns the
  one-time token), `GET /admin/api-keys?tenant_id=…` (metadata only, never a secret), and
  `DELETE /admin/api-keys/{id}` (idempotent revoke) — sits behind the super-admin session guard
  ([ADR-0034](0034-super-admin-auth.md)). An unknown scope name on provisioning is a `400`, not a
  silent drop (the deny-by-default read tolerance would wrongly issue a key granting nothing).
- **Deliberately not here yet:** a tenant self-service surface for managing its own keys (all
  provisioning is super-admin-driven for now), and per-key usage metering.

## Amendment 1 — a store's key names its store (2026-09-05)

**What was wrong.** "Every key is bound to one tenant" was the whole isolation story, and the
`/sync/stores/{store_id}/…` routes took the tenant from the verified grant and the store from the
**path**. Within one tenant that is not a check: every shop in a chain shares a tenant, so any
store's key served a sibling's configuration — including the `permissions` node, which carries
employee names and PIN hashes ([ADR-0070](0070-people-and-access.md)). No production tenant existed
when this was found, so it was a vulnerability rather than an incident.

**What changed.** `api_keys` gains a nullable `store_id` (migration `0047`), `StoredApiKey` and
`Grant` carry it, and `Grant::store()` exposes it. Two guards use it:

- `require_store` — the `/sync/stores/{id}` routes (config, heartbeat, report, artifact, devices,
  order relay pull and ack). The grant must name **this** store. A grant naming another, or naming
  none, is a `403`.
- `confine_to_store` — `/v1/stores/{id}/rollups/daily` and its window reads. An unbound key passes,
  because a tenant-wide integration key is the documented credential there; a *store's* key is still
  held to its own store, so a shop cannot read a sibling's takings with its rollup scope.

**Why `None` is refused on `/sync` rather than waved through.** Treating "names no store" as "may
act for any store in the tenant" would leave the finding open under a different name — the first
box provisioned with an old-style key would restore the exact behaviour being removed. A tenant-wide
key stays correct for an integration reading a whole tenant; it simply is not a store's credential.

**Upgrade.** Existing keys keep working everywhere except `/sync`, where they are now refused. Re-issue
each store's key from the console with the store named (the guided wizard does this automatically;
the **API keys** screen has a store picker) and put the new token in the box's environment file.
