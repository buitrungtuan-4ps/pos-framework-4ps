# ADR-0005 — Country-neutral core, fiscalization plug-ins

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

**Context.** Invoice numbering, tax rules and reporting obligations differ by country and change by legislation. Baking Vietnamese rules into the core would make every other market a rewrite.

**Decision.** The core guarantees only a **gapless per-store receipt number**, allocated in the same SQLite transaction as the bill. All legal obligations — legal invoice numbers, signing, tax-authority submission, per-channel tax rates — live behind the `Fiscalization` port in per-country crates (`fiscal-vn` first). Locale packs carry currency, time zone, date and number formats, and receipt templates.

**Consequences.**
- Adding a country means adding a crate, not editing the core.
- The receipt number and the legal invoice number are distinct concepts and must never be conflated in code or reports.
- Business date uses the store's cut-off; fiscal documents use the calendar date.
