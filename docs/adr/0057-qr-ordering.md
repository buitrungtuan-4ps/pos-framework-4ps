# ADR-0057 — QR ordering: a signed table id and a pure guardrail decision, over the public intake

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-21
**Relates to** [ADR-0012](0012-qr-ordering-via-cloud.md) · [ADR-0038](0038-webhook-tls-sender.md) · [ADR-0048](0048-ota-rollout-model.md) · [ADR-0056](0056-public-order-intake.md)

**Context.** [ADR-0012](0012-qr-ordering-via-cloud.md) fixed the shape of QR ordering: a guest scanning
a table code is on mobile data, not the store LAN, so the feature is cloud-served and submits through
the same `POST /v1/orders` path and `OrderIn` port the marketplaces use ([ADR-0056](0056-public-order-intake.md))
— which is what makes it nearly free. It also fixed the four protections a *static printed* QR code
needs, because a printed code is world-readable and never expires: **staff confirmation on by
default**, **per-table rate limits**, **business-hours-only**, and **rejection when the store is
offline** — plus a **signed `table_id`**, so a passer-by cannot forge one and order forty pizzas to a
table they are not sitting at. This ADR fixes how those are represented in code.

The guest has no API key — the QR itself is the credential — so the tenant binding
[ADR-0056](0056-public-order-intake.md) does through a bearer key cannot apply here. The signed
`table_id` is what replaces it: a token the store's admin minted, carrying the tenant, store, and
table, that the cloud verifies before it will accept anything.

**Decision.**

- **The `table_id` travels as an HMAC-signed token: `{tenant}.{store}.{table}.{hex_tag}`.** The tag is
  `HMAC-SHA256(secret, "{tenant}.{store}.{table}")`, on the exact `hmac`/`sha2` line and idiom the
  webhook signer already uses ([ADR-0038](0038-webhook-tls-sender.md)) — no new dependency, and the
  comparison is constant-time (`Mac::verify_slice`) so a verifier cannot leak the expected tag a byte
  at a time. `mint_table_token` is the admin side (printed into the QR); `verify_table_token` returns
  the `TableRef { tenant, store, table }` the intake needs, or refuses. A forged or tampered token is
  `BadSignature`; a structurally wrong one is `Malformed`. The token binds all three ids, so a valid
  token for one store's table can never be replayed as another's.

- **The guardrails are one pure, total decision: `evaluate(QrFacts) -> QrDecision`.** Like
  `decide_rollout` ([ADR-0048](0048-ota-rollout-model.md)), the policy is a pure function over facts
  the caller gathers, in a fixed precedence that is the safety argument:
  1. **token invalid → `UntrustedTable`.** A forged QR is refused before anything else, online or not.
  2. **store offline → `StoreOffline`.** The guest page says "please ask a member of staff"
     ([ADR-0012](0012-qr-ordering-via-cloud.md)); staff service is always the fallback.
  3. **outside business hours → `OutsideBusinessHours`.**
  4. **per-table limit reached → `RateLimited`.**
  5. otherwise **`Accept { require_staff_confirmation }`**, defaulting to `true`.

  Keeping it pure means every branch is a test with no clock, socket, or config reader, and the
  facts — is the store online, is it within hours, how many orders has this table placed — are
  gathered by the endpoint from the seams that own them (the store link, the config tree, a rate
  limiter), never embedded here.

- **Acceptance flows into the existing intake unchanged.** Once the token verifies and the guardrails
  pass, the QR submission becomes an `InboundOrder` with `sales_channel = Qr` and the token's
  `table_id`, and goes through the same `OrderIn` path as every other channel
  ([ADR-0056](0056-public-order-intake.md)). Staff confirmation is surfaced by
  `OrderAcceptance::awaiting_staff_confirmation`, which the store's implementation already sets for a
  table order; the guardrail's `require_staff_confirmation` is the cloud-side policy that it must.

**Rejected.**

- **An unsigned or opaque `table_id`** — rejected: a printed code is world-readable, so without a
  signature anyone could submit to any table. The signature *is* the QR's authentication.
- **A time-limited (expiring) token** — rejected for v1: the QR is printed and permanent, so expiry
  would break it daily. The rate limit and business-hours window bound abuse instead; an expiring,
  per-session token is a possible v2 for dynamic on-screen codes, not foreclosed.
- **Embedding the clock, config, and rate-limiter reads in the decision** — rejected: it would make
  the policy untestable without standing all three up, exactly the coupling the pure-decision shape
  avoids everywhere else in the domain.
- **A second submission pipeline for guests** — rejected by [ADR-0012](0012-qr-ordering-via-cloud.md)
  already: QR reuses `POST /v1/orders`/`OrderIn`, which is the whole reason it is cheap.

**Consequences.**

- **The security keystone and the policy are both proven in the fast gate**, with no new dependency:
  the token mint/verify (known-vector, tamper, wrong-secret, cross-store replay, malformed) and every
  guardrail branch and precedence are unit tests.
- **The endpoint that gathers the facts and relays to `OrderIn`** — reading the store's online status,
  the business-hours from config, and a per-table rate limiter — lands with the P11a cloud→store relay
  (P11b-2), the same buildable follow-on `POST /v1/orders` waits on; neither is externally blocked.
- **Nothing is foreclosed.** A dynamic per-session token, a different rate-limit strategy, or a
  configurable staff-confirmation default all sit behind these seams.
