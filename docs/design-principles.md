# Design principles

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

The mechanical rules in [`AGENTS.md`](../AGENTS.md) §2 are the enforceable subset of four principles. When a rule does not cover your situation, reason from here. Each principle is stated as *what it means in this codebase*, *what a violation looks like*, and *how it is checked* — not as theory.

## SOLID, translated

| Principle | What it means here | Violation smell | Checked by |
|---|---|---|---|
| **Single responsibility** | One crate, one reason to change. `pos-core` changes when business rules change; an adapter changes when its vendor changes; `ui/` changes when screens change | A module that both computes a bill total and talks to a printer | Crate boundaries + dependency allow-list test |
| **Open/closed** | Extend by **adding** an adapter or a country crate, never by adding a branch inside the core | `if country == "VN"` or `match vendor` anywhere in `pos-core` | Review + CI grep for vendor and country names in core |
| **Liskov substitution** | Every implementation of a port must pass that port's contract test suite. If a caller has to special-case one implementation, the *port* is wrong, not the caller | An adapter that "mostly" implements `EventStore` but needs the caller to skip a step | Shared contract tests per port |
| **Interface segregation** | Ports are small and role-shaped. `PrinterDriver` knows nothing about payments; `ClockSource` does nothing but tell the time | An adapter forced to implement empty or `unimplemented!()` methods | Review: empty implementations mean split the port |
| **Dependency inversion** | The core depends only on traits it owns; binaries wire concrete adapters ([ADR-0006](adr/0006-ports-and-adapters.md)) | `pos-core` importing `sqlx`, `reqwest`, or `tokio` | Dependency allow-list test in CI |

## KISS — boring by default

Complexity must be *justified in writing*, not assumed. This is why the system is two binaries with no orchestrator; why webhooks are a cursor over the event log instead of a queue system; why dashboards read rollup tables instead of an analytics engine; and why there is no cache in front of a query that already answers in under 10 ms.

Adding any infrastructure component requires answering the four admission questions in [`architecture.md`](architecture.md) Appendix B: what does it replace, what number proves we need it, what does it cost in RAM and processes, and can we remove it if we were wrong. No four answers, no component.

## DRY — about knowledge, not about lines

There is exactly one source of truth per concept: the permission registry, the event catalogue, the naming standard, the configuration tree, the capability flags, and an OpenAPI document generated from code. Never maintain a second copy by hand.

The inverse matters just as much: **code that looks similar but expresses different rules stays separate.** `fiscal-vn` and a future `fiscal-jp` will resemble each other and must not be merged into a "generic fiscal" abstraction — they change for different reasons, in different legislatures. That is single responsibility overruling superficial duplication.

**Rule of three:** extract shared code on the third occurrence, not the second. The adapter template is written after the third adapter exists, not before the first.

*Smell:* a shared helper with a boolean parameter that switches behaviour for its two callers. That is two functions wearing one coat.

## YAGNI — deferred, with a trigger

Nothing speculative is built. Every deferred capability has a **written activation threshold** instead of placeholder code:

| Deferred | Activates when |
|---|---|
| OAuth2 and a developer portal | A public marketplace exists |
| Container registry instead of `docker save` over SSH | More than one cell is deployed |
| Full RFC process, review SLA, coverage gate | More than three full-time engineers |
| Batched webhook delivery | A tenant exceeds roughly 500 events/second |
| Gift cards | The online balance ledger is genuinely needed (payment-method slot is reserved) |
| Retail UI profile | Retail pilot is scheduled — the data model already carries SKU, barcode, variant |
| Automated fork-to-UI end-to-end test on a virtual VPS | The fleet passes 50 stores |

Product features excluded on purpose, each with a reason, are listed in [`pos-spec.md`](pos-spec.md) §19.

*Smells:* an abstraction with exactly one implementation and no boundary rationale; a configuration flag nobody sets; "phase 2" code merged early.

*Stated exception:* ports begin life with a single implementation each. They exist because the **boundary itself is a product requirement** — portability across countries and vendors, plus millisecond domain tests ([ADR-0006](adr/0006-ports-and-adapters.md)) — not because a second implementation is imagined. This is the one place where we pay abstraction cost up front, and it is written down so nobody has to relitigate it.

## When principles collide

| Tension | Resolution |
|---|---|
| DRY versus YAGNI | Rule of three wins. Duplicate twice, extract on the third |
| SOLID versus KISS | Do not create an interface for a single implementation **unless** it is a deliberate boundary (a port) |
| Open/closed versus KISS | A plain `match` inside a binary beats a plugin system until the third case arrives |
| Any conflict costing more than a day of debate | Stop arguing, write an ADR |

## Reviewer checklist

1. Does this add a branch to the core that should have been an adapter?
2. Is this the third occurrence, or is it premature extraction?
3. New abstraction with one implementation — is there a boundary rationale?
4. New infrastructure component — are the four admission answers in the ADR?
5. Does the port stay small, or did it grow a method for one caller's convenience?
6. Do contract tests still cover every implementation?
7. Is money integer, time from the port, every queue bounded?
8. Are new names compliant, and are schema changes additive?
9. Did the documentation change land in this same PR?
10. Is there a changelog entry, with an upgrade note if protocol, migration, permission, or a default value changed?
