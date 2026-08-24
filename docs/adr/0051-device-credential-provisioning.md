# ADR-0051 — Device-credential provisioning: the cloud activation exchange

**Status** Accepted · **Owner** @maintainers-security · **Last reviewed** 2026-08-21
**Relates to** [ADR-0003](0003-cattle-not-pets.md) · [ADR-0013](0013-async-strategy.md) · [ADR-0016](0016-postgres-access.md) · [ADR-0037](0037-api-keys.md) · [ADR-0050](0050-activation-code-exchange.md) · `docs/roadmap.md` P9

**Context.** [ADR-0050](0050-activation-code-exchange.md) fixed the pure activation rule; P9 needs the
cloud endpoint that runs it. A super-admin issues a code bound to a device slot; a fresh machine
presents that code and receives a long-lived credential it stores in its `KeyVault`
([ADR-0003](0003-cattle-not-pets.md)). The exchange must be **single-use**, **atomic** (redeem and
mint together, or neither), and give an attacker **no oracle**. A wrinkle constrains the shape: the
cloud's store seams each check out their own pooled connection ([ADR-0016](0016-postgres-access.md)),
so "redeem the code and mint the credential" cannot be made atomic by composing two seam calls.

**Decision.**

- **The credential is an api-key-shaped bearer token.** `posdev_<id>_<secret>`: a ULID `id` (the public
  half, looked up when the credential is later presented) and a 256-bit CSPRNG `secret`. Only
  `SHA-256(secret)` is stored — not Argon2, because the secret is already high-entropy and there is no
  password to slow down, exactly [ADR-0037](0037-api-keys.md)'s reasoning — and the token is shown
  **once**, over TLS. The activation code itself is stored only as `SHA-256` of its canonical text, so
  a database dump leaks neither a live code nor a usable credential.

- **Redeem-and-mint is one adapter method in one transaction.** `consume_and_provision` runs
  `UPDATE activation_codes SET status = 'redeemed' WHERE code_hash = $1 AND status = 'issued'
  RETURNING <slot>` — the row count is the single-use guard — and, in the **same** transaction, inserts
  the `device_credentials` row for the slot the `RETURNING` gave back. Composed seam calls cannot share
  a transaction ([ADR-0016](0016-postgres-access.md)), so this is deliberately one purpose-built method,
  modelled on how `EventStore::append` drives multi-statement work over a single connection. The
  credential therefore can never inherit a slot other than the code's, and a crash between the two
  writes leaves neither.

- **One refusal, no oracle.** A spent code, a revoked code, an unknown code, and a code that lost the
  redemption race all return the same `403` with no detail; the reason is logged server-side only. A
  *malformed* code is the one exception — a plain `400` — because it never named a real code, so it
  leaks nothing. This is the posture of the api-key check ([ADR-0037](0037-api-keys.md)) and of the
  redemption verdicts ([ADR-0050](0050-activation-code-exchange.md)).

- **The cloud mints; the edge emits the event.** The exchange writes only the two activation tables; it
  does **not** append to the event log. `device.activation.completed` (`pos_proto::events`) is emitted
  by the *edge* once it has actually stored the credential (P9e) — the cloud has no path for minting a
  domain event onto the log, and the activation is not a fact until the box holds its credential.

- **The exchange route is unauthenticated; the code is the credential.** `POST /activate` carries no
  session or bearer — a fresh box has neither. The single-use 55-bit code is the authorisation. The
  admin issue and revoke routes stay behind the super-admin session guard
  ([ADR-0034](0034-super-admin-auth.md)).

- **The tables carry no row-level security.** Like `api_keys` ([ADR-0037](0037-api-keys.md),
  [ADR-0008](0008-postgres-partitioning.md)), a code is looked up by hash and a credential by id — a
  global key known before any tenant is proven — so isolation rests on the row's own `tenant_id`, bound
  to the action, not on an RLS predicate keyed off a tenant the caller has not yet established.

**Rejected.**

- **Composing separate `redeem` + `insert-credential` seam calls** — rejected: they run on different
  pooled connections and cannot be atomic, so a failure between them either spends a code without
  minting a credential (a bricked activation) or mints an orphaned credential. One transactional method
  is the only correct shape.
- **Argon2-hashing the credential secret** — rejected for the same reason as api keys: a high-entropy
  random secret gains nothing from a slow KDF, which would only add latency to every device
  authentication ([ADR-0037](0037-api-keys.md)).
- **Telling the caller why a code was refused** — rejected: distinguishing spent from revoked from
  unknown is an enumeration oracle; a single generic refusal removes it.
- **Emitting `device.activation.completed` cloud-side inside the exchange transaction** — rejected:
  activation is not complete until the edge has stored the credential, and the cloud has no event-minting
  path; the edge owns both the fact and the event.

**Consequences.**

- `pos-cloud` gains an `activation` seam (`ActivationCodeStore`, `IssuedCode`, `DeviceCredential`,
  `hash_code`, `mint_device_credential`) and three routes — `POST /admin/activation-codes`,
  `POST /admin/activation-codes/revoke`, and `POST /activate` — on their own sub-router, merged like
  `device_router`. `store-postgres` gains `activation_codes` and `device_credentials` (migration 0009)
  and the `PostgresActivationCodes` adapter with the transactional `consume_and_provision`. `pos-core`
  gains `ActivationCode::from_entropy`, so code generation stays at the I/O edge.
- Proven by: the pure redemption precedence (`pos-core`), the seam's hash-stability and
  secret-redaction unit tests, a router test binding single-use and the no-oracle refusal over a fake,
  and the `store-postgres` round-trip (issue → atomic redeem+mint → replay-refused → revoke) on real
  PostgreSQL.
- Deliberately elsewhere (P9e): the edge client that presents the code over `MessageLink`, stores the
  credential via `KeyVault`, and emits `device.activation.completed`; and the device-credential
  *verification* path for authenticated device calls, which will reuse `device_credentials` the way the
  api-key check reuses `api_keys`.
