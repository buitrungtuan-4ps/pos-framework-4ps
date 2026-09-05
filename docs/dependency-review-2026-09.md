# Dependency review — September 2026

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-05
· Covers the fifteen open Dependabot pull requests (#75–#89)

Every open Dependabot pull request, assessed against one question: **does this make a POS framework
that runs unattended in shops safer, or only newer?** A dependency that a thousand store machines
link is not a place to take an upgrade on faith, and it is also not a place to sit on a known-bad
version. Each row says which, and why.

Each was applied and **built and tested**, not read about. Where a bump is declined the reason is a
measured fact, not a preference.

## Summary

**Twelve of fifteen taken. Three declined**, on two distinct grounds, both of which would have cost
more than the upgrade was worth.

| PR | Change | Verdict |
|---|---|---|
| #75 | `actions/checkout` 4.2.2 → 7.0.1 | **Taken** |
| #76 | `actions/upload-artifact` 4.6.2 → 7.0.1 | **Taken** |
| #85 | `hyper` 1.11.0 → 1.11.1, `deadpool-postgres` 0.14.1 → 0.14.2 | **Taken** |
| #86 | `rusqlite` 0.32.1 → 0.40.2 | **Taken** |
| #87 | `tower-http` 0.6.11 → 0.7.1 | **Taken** |
| #88 | `ed25519-dalek` 2.2.0 → 3.0.0 | **Declined** — would put two Ed25519 implementations in one binary |
| #89 | `tokio-tungstenite` 0.29.0 → 0.30.0 | **Taken** |
| #78, #79 | `intl-messageformat` 10.7.18 → 11.2.14 | **Taken** |
| #80, #83 | `vite` 6.4.3 → 8.2.2 | **Taken** |
| #81, #82 | `@solidjs/router` 0.15.4 → 1.0.0 | **Taken** |
| #77, #84 | `typescript` 5.9.3 → 7.0.2 | **Declined** — v7 ships no compiler API, which four CI gates use |

The review also turned up a problem Dependabot could not raise, in a dependency **this repository
added days ago**. It is in the last section, and it was the most valuable finding of the exercise.

---

## Taken

### #75, #76 — the two GitHub Actions

Both are SHA-pinned and the pins move together with the version comment, so `cargo xtask
actions-pinned` still passes. These run only in CI: nothing reaches a store.

The reason to take them is concrete rather than hygienic. The current CI log says:

> `actions/checkout@11bd719…` target Node.js 20 but are being forced to run on Node.js 24

GitHub has deprecated the Node 20 runtime. Staying on v4 means every workflow run depends on a
compatibility shim GitHub has announced it will remove.

**Cost of taking:** none measured — every job passes.
**Cost of not taking:** CI breaks on GitHub's timetable rather than ours.

### #85 — `hyper` 1.11.1 and `deadpool-postgres` 0.14.2

Patch releases, and `hyper`'s are not cosmetic. Two of the four fixes are HTTP/1 parsing corrections:
recognising `\n\r\n` as a header terminator in the partial-read fast path, and detecting
`TE: trailers` case-insensitively. Disagreements between a proxy and an origin about where a request
ends are the request-smuggling family of bugs. `pos_cloud` sits behind Caddy, which is exactly that
shape.

**Cost of taking:** none. **Cost of not taking:** carrying a parser the maintainers have corrected.

### #86 — `rusqlite` 0.32.1 → 0.40.2

The largest single jump — eight minor versions on the edge's **event store**, which is where a
store's money lives. It earned the most scrutiny and produced the least drama: **no source change
was needed**, and all 52 `store-sqlite` tests pass, including the contract suites the port defines.

It also **paid a debt**. `deny.toml` carried a documented skip for a duplicate `hashbrown`, caused by
`rusqlite`'s `hashlink` pinning 0.14 while the cloud's `indexmap` was on 0.17. On 0.40 that duplicate
is gone, and `cargo deny` now reports the skip as unmatched. It has been deleted — and so has the
`syn@2` skip, which the ecosystem also finished migrating past. The skip list ratcheted from eleven
entries to nine, which is the direction the file's own comment asks for.

**Cost of taking:** a longer build, once. **Cost of not taking:** drifting further from a C library
(SQLite) whose fixes matter, and keeping two dead exemptions in the security policy.

### #87, #89 — `tower-http` 0.7.1, `tokio-tungstenite` 0.30

Both are middleware and transport under the HTTP stack. Both compiled and tested with **no source
change** in either binary. Nothing more to say, which is the point of reporting it.

### #78–#83 — `vite` 8, `@solidjs/router` 1.0, `intl-messageformat` 11

Three majors across both front ends, applied together and verified through the **whole** build chain
— not just `vite build`, but the four gates in front of it: `tsc --noEmit`, the no-hardcoded-strings
i18n lint, i18n parity across locales, WCAG-AA contrast, and the tap-count step budget. All pass in
both apps with no source change.

Vite 8 also made the till **smaller**: 41.74 kB → 39.61 kB gzipped. On a store tablet on a slow
connection, that is a real if modest win.

**Cost of not taking:** Vite 6 falls out of support, and `@solidjs/router` 0.15 is a pre-1.0 line
that will stop getting fixes now that 1.0 exists.

---

## Declined

Both refusals are about the same thing: an upgrade that is fine in isolation and expensive in this
tree.

### #88 — `ed25519-dalek` 2 → 3: two signature libraries in one binary

`ed25519-dalek` verifies the **minisign signature on an over-the-air update**. It is the check that
decides whether a store installs a binary. There is no dependency in the tree where a mistake is
worse.

The bump itself is clean: it compiles with no source change, and all six `updater-minisign` tests
pass — including the ones that matter (`verifies_a_valid_signature`, `rejects_a_tampered_artifact`,
`distinguishes_a_wrong_key_from_a_bad_signature`, `is_total_over_hostile_input`).

The problem is the rest of the graph:

```
ed25519-dalek v2.2.0
└── nkeys v0.4.5
    └── async-nats v0.50.0
        └── link-nats → pos-cloud, pos-edge
```

`async-nats` pins Ed25519 at 2 through `nkeys`. Taking 3 does not replace 2; it **adds** it. The
`pos_edge` binary would then link two independent Ed25519 implementations — and two
`curve25519-dalek`s under them — one verifying update signatures, the other authenticating to the
message broker.

`deny.toml` sets `multiple-versions = "deny"`, so this fails the gate. That gate could be silenced
with a skip, and this is precisely the case it exists to catch: a skip here would double the
cryptographic code a security review has to cover, for no gain a store can perceive.

And there is no gain. **`ed25519-dalek` 2.2.0 carries no advisory** — `cargo deny check advisories`
is clean on it. Our use is verifying a detached Ed25519 signature, which is the same operation in
both versions.

**Recommendation:** hold at 2, and revisit when `async-nats` moves. If the owner prefers to take it
anyway, the change is one line plus a `deny.toml` skip; the cost is the doubled crypto surface, not
a broken build.

### #77, #84 — TypeScript 5.9 → 7.0: the compiler API is gone

TypeScript 7 is the native (Go) rewrite, and `tsc --noEmit` passes on our code. The application is
not the problem.

The problem is that the v7 npm package exposes **only** `version` and `versionMajorMinor`:

```
$ node -e "import('typescript').then(m => console.log(Object.keys(m.default)))"
[ 'version', 'versionMajorMinor' ]
```

`createSourceFile`, `ScriptTarget`, `ScriptKind` — the entire AST API — are absent. Four of our CI
gates parse `.tsx` with it:

- `ui/scripts/i18n-lint.mjs` and `dashboard/scripts/i18n-lint.mjs` — the **no-hardcoded-strings gate
  ADR-0020 requires**, which is what stops an untranslated Vietnamese string shipping to a till.
- `ui/scripts/step-budget.mjs` and `dashboard/scripts/step-budget.mjs` — the tap-count budget.

Taking TypeScript 7 today means deleting an ADR-enforced gate, or rewriting all four against a
different parser. Neither is worth doing to be on 7 a few months early, and the framework is the
wrong place to trade a correctness gate for a version number.

**What was done instead:** TypeScript moved **5.7.3 → 5.9.3**, the newest 5.x. That is a real
improvement the repository was behind on, it keeps all four gates, and the whole build passes.

**Recommendation:** revisit when the TypeScript team ships the JS API for v7. There is no 6.x, so
this is the only intermediate position available.

---

## What Dependabot could not tell us

`cargo deny check advisories` fails on the tree **as it stood before this review** — not because of
anything Dependabot proposed, but because of a dependency added in the printing work
([ADR-0102](adr/0102-printing-any-script.md)):

| Advisory | Crate | |
|---|---|---|
| RUSTSEC-2026-0206 | `rustybuzz` | unmaintained; the maintainer names `harfrust` as successor |
| RUSTSEC-2026-0192 | `ttf-parser` | unmaintained; the author names the `fontations` project |

Both are *unmaintained* rather than *vulnerable*, and both sit in a font parser handling
operator-installed files rather than attacker-supplied ones — so the immediate risk is low. The
long-term risk is not: a font parser that will never be fixed again is a bad thing to build a
printing path on, and `deny.toml` sets `ignore = []` deliberately so that this cannot be waved
through with a comment.

**Fixed rather than ignored.** `pos-render` moved to the successors the advisories themselves name:

- `rustybuzz` → **`harfrust`** 0.13.3, the HarfBuzz project's own maintained Rust port
- `ttf-parser` → **`skrifa`** 0.46.2, part of Google Fonts' `fontations`

They share one font parser (`read-fonts`), so the tree gains no second one; both are MIT/Apache and
pure Rust, so the cross-compilation posture is unchanged. All 20 `pos-render` tests pass, and the
Devanagari output was compared glyph-for-glyph against the previous stack — the headline still joins
across the word and the `ि` matra still reorders ahead of its consonant.

`cargo deny check advisories` is now **ok**, as are `bans` and `licenses`.

This is the argument for running `cargo deny` on a schedule rather than only reacting to Dependabot:
Dependabot proposes *newer*, and it has nothing to say about a crate whose latest version is also its
last.

## Verification

`cargo build --workspace --all-targets` · `cargo test --workspace` (91 targets, all green) ·
`cargo clippy --workspace --all-targets -D warnings` · `cargo fmt --all --check` ·
`cargo deny check advisories | bans | licenses` (all ok) · `cd ui && pnpm build` and
`cd dashboard && pnpm build` (both, through every gate) · the xtask gates including
`actions-pinned` and `deps-rule`.
