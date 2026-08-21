# ADR-0047 — Minisign update verification: `ed25519-dalek` + `blake2`, verify-only

**Status** Accepted · **Owner** @maintainers-security · **Last reviewed** 2026-08-21
**Relates to** [ADR-0007](0007-in-house-vs-dependency.md) · [ADR-0021](0021-corrected-port-list.md) · [ADR-0026](0026-port-shapes.md) · `docs/architecture.md` §4 · `docs/roadmap.md` P9

**Context.** The [`Signer`](../../crates/pos-ports/src/signer.rs) port has existed since P2 —
verify-only, two baked-in keys, an 8-byte key id read from the signature, revocation carried by the
config tree, and a contract suite (`pos-contract-tests`) that the in-memory [`FakeSigner`] already
passes. P9 needs the **real** implementation: the concrete adapter that verifies a `pos_edge`/`pos_cloud`
release artifact was signed by an offline key before it is ever trusted. `docs/architecture.md` §4 fixes
the scheme as **minisign**, signed offline on a machine whose private key lives on a USB stick with a
paper copy in a safe. The open questions are which crates do the arithmetic, and exactly what bytes the
port's opaque `Signature` and `PublicKey` carry.

**Decision.**

- **Verification uses `ed25519-dalek` for Ed25519 and `blake2` for the prehash — both pure-Rust, no C,
  no new crypto of our own** ([ADR-0007](0007-in-house-vs-dependency.md): cryptography is the canonical
  buy-not-build). `ed25519-dalek` 2.x is the audited, maintained Dalek implementation (the double-public-key
  oracle RUSTSEC-2022-0093 was a 1.x issue, fixed in 2.x); `blake2` is the RustCrypto BLAKE2b used only
  to reproduce minisign's prehash. The adapter is a new workspace crate,
  `crates/adapters/updater-minisign`, implementing `Signer` and nothing else — **there is no `sign`
  method anywhere**, so a compromised store binary carries no key material and no code that could sign.

- **The port's `Signature` is minisign's binary signature blob: `algorithm(2) ∥ key_id(8) ∥
  ed25519_sig(64)` = 74 bytes** — the base64-decoded first signature line of a `.minisig` file. `key_id_of`
  reads bytes `2..10` without trusting anything, which is what lets a **revocation check run before the
  artifact is trusted**. `algorithm` is `Ed` (legacy: Ed25519 over the raw artifact) or `ED` (prehashed:
  Ed25519 over `BLAKE2b-512(artifact)`, minisign's default for large files); both are verified, anything
  else is malformed. The port's `PublicKey` carries the 8-byte key id (as `KeyId`) and the raw 32-byte
  Ed25519 public key (as the bytes), matching the tail of a decoded minisign `.pub`.

- **The three status distinctions the port fixed are honoured exactly** ([ADR-0026](0026-port-shapes.md) §5,
  and the reason the suite exists). A signature whose embedded key id is not the key it is checked against
  is `invalid_argument` — "try the other baked-in key", the whole point of a two-key rollout — never a
  verification failure. A well-formed signature for the right key that does not verify is
  `permission_denied` — terminal, an update that must never be auto-retried into installation. Malformed,
  truncated, empty, or hostile bytes are `invalid_argument`, and **never panic**: the crate inherits the
  backbone lints (`indexing_slicing`, `panic`, `unwrap_used`, `expect_used` all denied), so every parse is
  `slice.get(..)` returning an error, checked rather than promised — verification runs at startup on bytes
  an attacker chose, before anything else works.

- **The production keys are not in this repository.** Generating the two minisign keypairs and storing them
  offline is the human-only step `docs/roadmap.md` P0/P9 reserves; the adapter only ever holds *public*
  keys, which the binary bakes in and the cloud's revocation list gates. The tests sign with throwaway
  keypairs generated from fixed seeds — real Ed25519, real signatures, real verification, but never a
  production key — which is exactly enough to pass the `Signer` suite and prove the arithmetic.

**Rejected.**

- **Hand-rolling Ed25519 or BLAKE2b** — rejected outright ([ADR-0007](0007-in-house-vs-dependency.md)):
  bespoke curve arithmetic on the one path that gates every update is the worst possible place to be
  clever. We take the audited crate.
- **A framework-specific signature format instead of minisign** — rejected: the offline signing workflow
  uses the `minisign` CLI, so the on-disk format has to be minisign's, or the humans holding the keys
  cannot produce a signature this code accepts.
- **`ring`/`aws-lc-rs` for Ed25519** — rejected here: those pull a C/asm crypto core, whereas the store
  binary's whole posture is pure-Rust and reproducible; `ed25519-dalek` keeps verification in the same
  no-C world as the rest of the edge. (`ring` remains the TLS provider on the cloud side, a separate
  concern — [ADR-0038](0038-webhook-tls-sender.md).)

**Consequences.**

- `crates/adapters/updater-minisign` joins the workspace, implementing `Signer` and passing the existing
  suite; `ed25519-dalek`, `curve25519-dalek`, and `blake2` enter the dependency graph. They pin the
  `sha2 0.10` / `digest 0.10` line (Ed25519 hashes with SHA-512), which now coexists with the `sha2 0.11`
  the cloud webhook HMAC adopted — a duplicate major the `cargo-deny` `multiple-versions` gate flags, so
  `deny.toml` gains dated, reviewed `skip` entries for the RustCrypto 0.10 line exactly as it already
  carries for `syn@2` / `sha2@0.11`. No shipped binary links two copies of the *same* code path; the
  duplication is build-time only, across two independent uses.
- What this adapter deliberately does **not** do is decide *whether a valid key is still trusted* (that is
  the revocation list, [`ConfigStore`](../../crates/pos-ports/src/config_store.rs)) or *how an update rolls
  out* (rings, self-test, rollback, kill switch — ADR-0048, next). It answers one question — "is this
  signature valid for this key?" — and answers it with real cryptography.
