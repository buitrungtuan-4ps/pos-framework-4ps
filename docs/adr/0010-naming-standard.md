# ADR-0010 — snake_case everywhere

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

**Context.** Google's API guidance is the closest thing to an industry standard, but it is internally split: proto fields are `snake_case` while the default JSON mapping is `lowerCamelCase`, and URL collection identifiers are `lowerCamelCase`.

**Decision.** One rule across the whole system: `snake_case` for JSON, URL path segments, database columns, event type names, configuration keys, metric names and permission identifiers. Timestamps end in `_time` (RFC 3339, UTC). Identifiers are `{resource}_id`. Money is `currency_code` plus integer `amount_minor`. Enum values are `UPPER_SNAKE_CASE` with a mandatory `*_UNSPECIFIED` zero value.

**Consequences.**
- Four deliberate deviations from Google AIP, documented in `naming-and-api.md` §12.
- The same concept carries the same name in every layer, so no mapping layer exists between API and database.
- A CI naming linter and three snapshot files (API, event schema, permission catalogue) enforce this mechanically.
