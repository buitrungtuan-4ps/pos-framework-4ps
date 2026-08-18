# ADR-0011 — Country lives in the hostname

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

**Context.** Expanding to a second country raises where the country label belongs. A path prefix (`domain.com/jp`) looks tidy, but a hostname resolves to exactly one place, so every Japanese request would land in the home country first and be forwarded.

**Decision.** Each country is an independent cell reached by hostname: the home country keeps the bare domain, additional cells use `*.jp.domain.com`. DNS is the router. In front of cells we only ever **redirect** (a tiny non-personal slug → country directory issues a 301), never **proxy**.

**Consequences.**
- Personal data never transits or is decrypted in another jurisdiction — the legal reason cells exist.
- No global single point of failure and no cross-region latency detour.
- Session cookies are isolated by hostname, which a path prefix cannot do.
- Users of expansion countries see a country label in the URL after the first redirect; bookmarks then point directly at their cell.
