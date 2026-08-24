# ADR-0001 — The store sells without the cloud

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

**Context.** Restaurants lose internet regularly: 4G drops, routers reboot, providers fail. A POS that stops selling during an outage is unusable, and outages cluster at peak hours.

**Decision.** All logic required to take an order, route it to the kitchen, take payment and print a receipt runs inside the store on `pos_edge` against local SQLite. The cloud manages configuration, fleet health, reporting and integrations, and is never consulted to complete a sale. Events reach the cloud asynchronously through a durable outbox.

**Consequences.**
- Cloud downtime costs administration and cloud-only features, never revenue.
- Every feature must answer "what happens offline?"; cloud-only features (voucher redemption, QR ordering, delivery-app intake) degrade explicitly rather than blocking the till.
- Reports are eventually consistent, typically 1–3 s behind.
- Uniqueness that must be global (voucher redemption, receipt sequence across a replacement machine) needs a separate mechanism — see ADR-0003 and the pricing engine rules.
