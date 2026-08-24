# ADR-0009 — Licence: proprietary, internal use

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

**Context.** The framework is designed to be forked and deployed, which raises the question of how it is licensed. Options considered: Apache-2.0 (widest adoption, patent grant), AGPL-3.0 (prevents closed re-hosting), BUSL/source-available (blocks competing commercialisation, converts later), or closed.

**Decision.** Closed and proprietary, for internal use, for now. `LICENSE` states exclusive copyright, every source file carries a header, and the repository stays private.

**Consequences.**
- No external contributor process is needed yet; the AI-contributor and review rules still apply.
- All technical decisions that keep open-sourcing *possible* remain in force: the dependency tree is free of AGPL and MPL, and `cargo-deny` continues to block copyleft. Reversing this ADR later requires no dependency surgery.
- Any decision to publish or license to third parties supersedes this record with a new ADR.
