# ADR-0133 — The backbone defines what is hashed, not how

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-22
**Relates to** [ADR-0006](0006-ports-and-adapters.md) (the rule the allow-list enforces) · [ADR-0013](0013-async-strategy.md) (why `pos-core` and `pos-ports` are sans-I/O) · [ADR-0131](0131-a-chained-event-log.md) (which split the preimage from the digest and left this question open)

**Context.** [ADR-0131](0131-a-chained-event-log.md) put the chain's **preimage** in `pos_proto::chain` — the exact bytes a chain hash is taken over, in the exact order — and left the **digest** to each tier that already links `sha2`. `pos-proto` is governed by `tools/backbone-allowlist.toml`, whose own comment says adding a name there "needs an architecture reviewer and an ADR", and ADR-0131 did not name one. That module says as much, and calls consolidating "a reasonable different answer that needs its own ADR". This is that record.

The cost has since become concrete. `ChainHash::of(Sha256::digest(preimage.as_bytes()).into())` now appears **four times** — `store-sqlite`'s writer, `pos-cloud`'s chain audit, the in-memory fake, and a test — each wrapped in a small private helper. [`docs/design-principles.md`](../design-principles.md) says to extract on the third occurrence.

**Options considered.**

1. **Allow-list `sha2` and move the digest into `pos-proto`.** The obvious reading of "extract on the third". **Rejected on a measurement**, not on principle:

   - With default features, `pos-proto`'s resolved closure gains **thirteen** crates, among them `getrandom`, `rand_core` and **`libc`**. A backbone crate one hop from an OS entropy source is the opposite of what [ADR-0013](0013-async-strategy.md) built, and sits badly beside the rule that the core reaches a random generator only through `IdGenerator`.
   - With `default-features = false` — the shape [ADR-0014](0014-datetime-library.md) already uses for `jiff` — it is **eight**: `sha2`, `cfg-if`, `cpufeatures`, `digest`, `block-buffer`, `hybrid-array`, `typenum`, `crypto-common`. Much better, and still rejected for two reasons.
   - **`cpufeatures` reads the machine.** That is its entire job: runtime CPU feature detection. The backbone's forbid-pass exists so a reader can say *these three crates touch nothing about the host*, and this would make that sentence false.
   - **It makes the backbone's dependency set target-dependent.** On `aarch64-linux`, `aarch64-android` and `aarch64-apple`, `cpufeatures` pulls `libc`. `cargo xtask deps-rule` resolves the closure for the host it runs on, so CI on `x86_64` would pass while an ARM64 store build carried a crate the gate never saw. A gate that is weaker than it looks is worse than a gate that refuses.

2. **A new crate that links `sha2` and holds the helper.** Rejected: a crate, a manifest and a place in the dependency graph, to hold one expression.

3. **Keep the digest out of the backbone, and move the *sequence* into it.** `pos-proto` already owns the preimage and `ChainHash`; what the four copies duplicate is the three-step sequence *preimage → digest → wrap*. Have `pos-proto` own that sequence and take the digest as an argument. This is the option taken.

**Decision.**

1. **`sha2` is not added to `tools/backbone-allowlist.toml`.** The question is settled by this record rather than left to be asked again; a later reader who wants it should supersede this ADR with a reason the measurement above does not already answer.

2. **`EventEnvelope::chain_hash(&self, link, digest)` lives in `pos-proto`**, building the preimage, applying the caller's digest and wrapping the result. The caller supplies `|bytes| Sha256::digest(bytes).into()` — one line naming the function it already links, with no decision in it.

3. **The three private helpers are deleted.** One definition of the sequence, and the dependency points the way the tiers already run: the backbone says what is true, and the tier that has the hardware does the work.

**Consequences accepted.**

- **Every tier still names `sha2` itself.** Four call sites become four closures rather than one import. That is the trade: it is the *sequence* that could drift, and after this it cannot, while *which digest* stays a choice each tier states out loud — which is the honest place for it, since a tier that wanted a different one would have to say so rather than inherit it.
- **The signature admits a wrong digest.** Nothing stops a caller passing a closure that returns a constant. Neither did the previous shape, and the contract suite ([ADR-0131](0131-a-chained-event-log.md) decision 5) is what catches it: a store whose hashes do not chain fails the obligations.
- **`pos-proto` gains no dependency**, and the forbid-pass keeps the sentence it protects: these three crates touch nothing about the host.
