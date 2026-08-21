# ADR-0050 — Activation-code exchange: single-use, locally checkable, credential into the vault

**Status** Accepted · **Owner** @maintainers-security · **Last reviewed** 2026-08-21
**Relates to** [ADR-0003](0003-cattle-not-pets.md) · [ADR-0013](0013-async-strategy.md) · [ADR-0049](0049-single-active-lease.md) · `docs/architecture.md` §4 · `docs/roadmap.md` P9

**Context.** A new or replacement machine boots holding no credential. P9's "cattle not pets" promise —
a dead mini-PC replaced in five minutes — rests on a machine getting its long-lived credential without
a credential-distribution ritual. `docs/architecture.md` §4 and [ADR-0003](0003-cattle-not-pets.md) fix
the flow: an operator activates the machine once with a **short code**, the machine exchanges that code
for its credential, and stores the credential in the operating system's protected store — the
`KeyVault` port, under `SecretName::DeviceCredential` (DPAPI or a TPM on Windows, the keyring on Linux).
The code is then useless. The wire already carries the surrounding pieces: a machine mid-activation
sends its `Hello` with no `lease_token` yet (`pos_proto::protocol`), and `device.activation.completed`
(`pos_proto::events`) is the audit event on success. What is undesigned is the **code's format** and the
**redemption rule** — and, per [ADR-0013](0013-async-strategy.md), the rule must be expressible without
the domain performing I/O.

**Decision.**

- **The activation code is a short, locally checkable string.** Eleven payload symbols (55 bits of
  entropy) plus a trailing checksum symbol, drawn from Crockford's base-32 alphabet — which omits `I`,
  `L`, `O`, `U`, the glyphs a human misreads — displayed as `XXXX-XXXX-XXXX`. `ActivationCode::parse`
  normalises input the way that alphabet is meant to be read (case-insensitive, hyphens and whitespace
  discarded, `I`/`L` folded to `1` and `O` to `0`) and verifies the checksum, so a mistyped code fails
  **at the keyboard** rather than after a network round-trip and an opaque error. The checksum is a typo
  guard, **not** authentication: it is one symbol, forgeable in a few tries. The security is the
  entropy of the code plus the single-use redemption below, with the cloud as the sole authority on
  whether a code is live.

- **Redemption is single-use and deny by default.** `redeem(status)` grants only a code the cloud still
  records as `Issued`; `Redeemed` and `Revoked` are refused, each with its reason. A grant obliges the
  caller to flip the record `Issued → Redeemed` **in the same transaction that mints the credential**,
  so a replayed code — the same setup sheet used twice — finds `Redeemed` and is refused. That
  atomicity is the cloud's transaction, exactly as an event append is atomic there
  ([ADR-0013](0013-async-strategy.md)); the domain owns the *rule*, not the transaction. `Revoked`
  exists for the leaked-sheet case: an administrator cancels a code before it is used, without needing a
  clock in the domain.

- **The credential lands in the vault, never a file.** On a grant the edge stores the returned
  credential through the `KeyVault` port under `SecretName::DeviceCredential`; that port already refuses
  to fall back to a file when the protected store is unreachable ([ADR-0003](0003-cattle-not-pets.md)),
  which is what stops a credential ending up in a backup. `device_activation(credential_present)` is the
  domain's one-line statement of the boot rule — a box holding that secret is activated and may present
  its lease ([ADR-0049](0049-single-active-lease.md)); one without it must run the exchange.

- **It is pure `pos-core`, naming no port.** `ActivationCode`, `CodeStatus`, `redeem`,
  `ActivationStanding` and `device_activation` are plain values and total functions, so the simulator
  (P12) can exhaust a first activation, a replay of a spent code, and a revoked code with no network.
  The code is treated as a bearer credential inside the domain: its `Debug` is redacted, and a single
  conspicuously named accessor yields the text.

**Rejected.**

- **A code with no checksum** — rejected: every fat-fingered character then becomes a pointless round
  trip and an error the operator cannot act on; a local check is nearly free and turns "it didn't work"
  into "you mistyped the seventh character".
- **Treating the checksum as authentication** — rejected: it is a single base-32 symbol, guessable in
  thirty-two tries. Conflating it with the real gate invites someone to lean on it and weaken the
  entropy or the single-use consume, which *are* the security.
- **Multi-use or purely time-boxed codes** — rejected: a code that works twice reintroduces the
  credential-distribution problem the one-time exchange exists to remove. Single-use with explicit
  administrator revocation covers the leaked-sheet case and keeps a clock out of the domain
  ([ADR-0013](0013-async-strategy.md)); if a time-to-live is ever wanted, the cloud collapses an expired
  code to a refused status when it loads the record, rather than the domain reading a clock.
- **Storing the credential in a file, an environment variable, or beside the binary** — rejected by the
  `KeyVault` port itself ([ADR-0003](0003-cattle-not-pets.md)); a silent file fallback is precisely how a
  credential reaches a backup or a log.

**Consequences.**

- `pos-core` gains an `activation` module — `ActivationCode` (with `CodeError`), `CodeStatus`,
  `RejectReason`, `Redemption`, `redeem`, `ActivationStanding`, and `device_activation` — with tests
  binding the checksum, the glyph folding, the length and alphabet checks, and the single-use
  precedence. No new dependency; no `pos-ports`.
- Deliberately elsewhere (P9e): the cloud endpoint that issues a code, looks it up, and consumes it in
  the credential-minting transaction; and the edge activation client that presents the code over
  `MessageLink`, receives the credential, stores it via `KeyVault`, and emits
  `device.activation.completed`.
