# ADR-0012 — QR ordering is a cloud module

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

**Context.** Guests scanning a table QR code are on mobile data, not the store LAN, so they cannot reach `pos_edge` directly. The feature therefore has to be served by the cloud, which conflicts with the offline-first principle.

**Decision.** Build QR ordering as a cloud-served web application that submits through the existing `POST /v1/orders` path and the `OrderIn` port, so the store receives guest orders exactly like delivery-app orders. Guest payment in version one is **pay at the counter**. Static printed QR codes are protected by staff confirmation (on by default), per-table rate limits, business-hours-only acceptance, and rejection when the store is offline.

**Consequences.**
- Architecturally almost free: no new component, no new pipeline.
- This is the first feature where cloud availability is visible to customers. When the cloud or the store link is down, the guest page says "please ask a member of staff" — staff service is always the fallback, and no end-customer SLA is promised.
- Serving menu images to guests makes **bandwidth** a real constraint for the first time; thumbnails, immutable caching and optionally a CDN for images only (images contain no personal data) keep it manageable.
