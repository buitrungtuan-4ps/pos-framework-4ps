# Naming and API standard

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

Based on Google's API Improvement Proposals (AIP), with four deliberate deviations listed in §12.

**The one rule:** `snake_case` everywhere that crosses a boundary — JSON, URLs, database columns, event names, configuration keys, metric labels, permission ids.

---

## 1. Principles

1. **One name per concept, everywhere.** `store_id` is `store_id` in the API, the database, events, logs, and documentation. No translation layers.
2. **Rust follows Rust internally** (`snake_case` fields, `PascalCase` types) and maps 1:1 to the wire with `#[serde(rename_all = "snake_case")]`.
3. **Names are contracts.** Once published, a name may only be added to, never renamed or removed — deprecate instead.
4. **No abbreviations** except: `id`, `url`, `api`, `sku`, `vat`, `qr`, `pos`, `kds`, `erp`, `ip`, `ttl`, `utc`.

## 2. Resources and identifiers

| Rule | Example |
|---|---|
| Collections are plural, `snake_case` | `/v1/orders`, `/v1/order_lines`, `/v1/price_lists` |
| Primary keys carry the full name; never a bare `id` | `order_id`, `store_id`, `menu_item_id` |
| Foreign keys keep the referenced key's name | `orders.store_id` → `stores.store_id` |
| Identifier values | ULID (26 characters, time-sortable) |
| Hierarchy is expressed as fields, not deep paths | `{"tenant_id":…, "brand_id":…, "store_id":…}` |

## 3. Fields

- `snake_case` nouns; no prepositions or articles: `discount_reason`, not `reason_for_the_discount`.
- Never encode the type in the name: `note`, not `note_string`.
- Arrays are plural: `payments`, `order_lines`.
- Booleans are adjectives or past participles with **no `is_` prefix**: `enabled`, `voided`, `fired`, `settled`. Capability flags use an `_enabled` suffix: `tables_enabled`, `tips_enabled`.
- Units are mandatory suffixes: `_time`, `_duration_ms`, `_count`, `_bytes`, `_ratio`, `_percent`, `_amount_minor`, `_weight_grams`, `_volume_ml`.

**Standard fields:** `business_date` (a `DATE`, derived from the store's day-cutoff hour — distinct from `event_time`) · `subject_id` (points to a separately stored PII record; **events never carry PII directly**) · `buyer_name`, `buyer_tax_code`, `buyer_email` (PII, masked in logs) · `counted_qty` with `count_time`.

### 3.1 Time

Every timestamp ends in `_time` and is **RFC 3339 UTC**: `create_time`, `update_time`, `event_time`, `fire_time`, `settle_time`, `close_time`. Durations state their unit: `prep_duration_ms`. `created_at` and `updated_at` are banned.

### 3.2 Money

```json
{ "currency_code": "VND", "amount_minor": 150000 }
```

`currency_code` is ISO 4217. `amount_minor` is an **integer** in the currency's minor unit (đồng for VND, yen for JPY, cents for USD). In the database: `bigint` plus `char(3)`. Floating point is banned at every layer.

### 3.3 Enums

`UPPER_SNAKE_CASE`, always with a zero value `*_UNSPECIFIED`:

```
ORDER_STATE_UNSPECIFIED · ORDER_STATE_OPEN · ORDER_STATE_SETTLED · ORDER_STATE_VOIDED
PAYMENT_METHOD_UNSPECIFIED · PAYMENT_METHOD_CASH · PAYMENT_METHOD_CARD ·
PAYMENT_METHOD_QR · PAYMENT_METHOD_VOUCHER · PAYMENT_METHOD_GIFT_CARD · PAYMENT_METHOD_OTHER
```

Receivers **must** treat unknown values as `*_UNSPECIFIED` instead of failing. That is what makes adding enum values a non-breaking change.

## 4. HTTP API

**Standard methods:** `GET /v1/orders` · `GET /v1/orders/{order_id}` · `POST /v1/orders` · `PATCH /v1/orders/{order_id}` (with `update_mask`) · `DELETE /v1/orders/{order_id}`.

**Custom methods use a colon** (AIP-136), which distinguishes actions from sub-resources:

```
POST /v1/bills/{bill_id}:void
POST /v1/bills/{bill_id}:refund
POST /v1/bills/{bill_id}:split
POST /v1/webhook_endpoints/{webhook_endpoint_id}:rotate_secret
POST /v1/webhook_deliveries/{webhook_delivery_id}:redeliver
```

**Pagination** (AIP-158): request `page_size` and `page_token`; response returns `next_page_token`.

`GET /v1/events` — the paged event feed this section used to give as the worked example — **does not
exist**, and the `read_events` scope that would have gated it was removed in roadmap **Q5**. Events
leave the cloud two other ways: pushed to a registered endpoint as webhook deliveries
([ADR-0032](adr/0032-webhooks.md)), or read from the store's own log on the box. A public pull feed
is a new read surface with its own PII, retention and paging decisions and is not promised here.
Paged `/admin` reads follow [ADR-0098](adr/0098-paged-admin-reads.md) instead, which is the
convention the cloud actually implements.

**Filtering and ordering:** `filter`, `order_by=create_time desc`. **Partial update:** `update_mask=display_name,price_amount_minor`.

**Headers**

| Header | Purpose |
|---|---|
| `idempotency-key` | Deduplicate creates (industry standard) |
| `pos-signature` | Webhook HMAC-SHA256, as `v1=<hex>` |
| `pos-signature-time` | Signing timestamp, Unix seconds (replay window ±5 minutes) |
| `pos-delivery-id` | Webhook idempotency key: the page being delivered, stable across retries |
| `pos-edge-version` | **Response** header on every edge `/api/*` answer: the release that replied ([ADR-0111](adr/0111-a-second-origin-may-address-the-edge.md)). Not `PROTOCOL_VERSION`, which is the edge↔cloud wire language and is §11's axis — an app is not on that wire |

HTTP header names use hyphens by convention — this is HTTP, not a violation of the snake_case rule.
They carry no `X-` prefix: [RFC 6648](https://www.rfc-editor.org/rfc/rfc6648) deprecated it for new
headers, and a receiver should not have to guess which spelling a version sends.

**This table is the contract, and the code is checked against it.** It listed two more headers until
roadmap **Q5**, and neither existed:

- **`pos-event-id`** described a webhook that delivers *one event*. It does not: a delivery is a
  **page** of events, read after the endpoint's cursor and re-sent unchanged until the receiver
  accepts it ([ADR-0032](adr/0032-webhooks.md)). There is no single event to name. A receiver
  dedupes on the `event_id` **inside** each event in the body, which every event envelope carries.
  It stays removed.
- **`pos-delivery-id`** was removed with it, and has since come back as the real addition Q5 said it
  would have to be (production-readiness **R6**). It names the **page**, not one event: a failed
  delivery leaves the cursor where it was, so the retry re-reads the identical page and re-signs it
  with a fresh timestamp — meaning the signature differs on every attempt and the two are otherwise
  indistinguishable except by hashing the body. The value is `{store_id}.{first_event_id}`, which is
  stable for as long as the page is (every retry of it) and changes the moment the cursor advances.
  It is **absent, never empty**, on a body that is not a cursor page — an alert notification carries
  no page and so keys on nothing.
- **`pos-api-version`** was an optional minor-version pin that nothing read. Every route ignored it,
  so an integrator who sent it believed they had pinned something and had not — worse than no header
  at all. Pinning is deliberately not being introduced now
  ([ADR-0094](adr/0094-console-optimistic-concurrency.md) says the same: `/v1` error bodies may
  change shape and `pos-api-version` does not move). `PROTOCOL_VERSION` remains the one negotiated
  version, on the edge↔cloud wire ([ADR-0024](adr/0024-protocol-version-negotiation.md)).

**Errors** (AIP-193) — one shape everywhere:

```json
{ "error": { "code": 400, "status": "INVALID_ARGUMENT",
             "message": "price_amount_minor must be positive",
             "details": [ { "field": "price_amount_minor", "reason": "MUST_BE_POSITIVE" } ] } }
```

Canonical statuses: `INVALID_ARGUMENT`, `NOT_FOUND`, `ALREADY_EXISTS`, `PERMISSION_DENIED`, `UNAUTHENTICATED`, `FAILED_PRECONDITION`, `RESOURCE_EXHAUSTED`, `UNAVAILABLE`, `INTERNAL`.

Three statuses are ours, not AIP's, and all three are safe because `status` parses openly — a client built before any one of them reads it as unrecognised and still gets an intact `code`, `message` and `details`:

| Status | HTTP | Why it exists |
|---|---|---|
| `UNSPECIFIED` | `500` | The naming standard's rule that every enum has one. Nothing emits it; it is what an older client sees in place of a status a newer server added. |
| `VERSION_MISMATCH` | `412` | A conditional write whose `If-Match` names a version the resource no longer holds ([ADR-0094](adr/0094-console-optimistic-concurrency.md)). No canonical status maps to `412`, and it is deliberately not called `PRECONDITION_FAILED` — beside `FAILED_PRECONDITION` (`409`) that would be two tokens differing only in word order with different codes. |
| `UNPROCESSABLE` | `422` | The request parsed, every field is individually valid, and the *combination* is not: a routing rule naming an unknown station, capability flags that violate a §10 inter-flag rule, a translation key missing its `en` fallback ([ADR-0096](adr/0096-unprocessable-status.md)). Distinct from `INVALID_ARGUMENT` (`400`), which tells a reader to fix the field named in `details`; this one tells them every field is fine and the document is still inconsistent. |

**Optimistic concurrency** (ADR-0094): a read that can be written back carries an opaque version — an `ETag` header on a single resource, an `etag` field per row on a list, byte-identical either way. A mutating request sends it back in `If-Match`; the header is **required**, its absence is an `INVALID_ARGUMENT` naming the `if-match` field, and a stale value is `VERSION_MISMATCH`. The token is opaque: never parse it, never compare two for ordering, never construct one.

## 5. Events

Event names are `domain.resource.action`, `snake_case`, action in the **past tense** — the same taxonomy as permission ids. The full catalogue is in [pos-spec.md](pos-spec.md) §18.

**Envelope** (identical on every channel):

```json
{
  "event_id": "01J...",
  "event_type": "billing.bill.settled",
  "event_time": "2026-08-14T03:12:45.123Z",
  "business_date": "2026-08-13",
  "schema_version": 1,
  "tenant_id": "…", "brand_id": "…", "store_id": "…",
  "device_id": "…", "employee_id": "…", "shift_id": "…",
  "data": { }
}
```

`event_id` is a ULID and doubles as the receiver's idempotency key. `schema_version` only increases when a break is unavoidable; by default every change is additive.

## 6. Database (PostgreSQL and SQLite share these rules)

| Object | Convention | Example |
|---|---|---|
| Table | plural, `snake_case` | `orders`, `order_lines`, `bill_payments`, `stock_ledger_entries` |
| Column | **identical to the JSON field name** | `store_id`, `create_time`, `amount_minor` |
| Primary key | `<resource>_id` | `order_id` |
| Index | `idx_<table>_<columns>` | `idx_orders_store_id_create_time` |
| Unique | `uq_<table>_<columns>` | `uq_bills_store_id_receipt_number` |
| Foreign key | `fk_<table>_<target_table>` | `fk_order_lines_orders` |
| Check | `ck_<table>_<rule>` | `ck_bill_payments_amount_minor_positive` |
| Partition | `<table>_p_<key>` | `events_p_2026_08` |
| Enum storage | `text` plus a check constraint, values identical to the wire | `'ORDER_STATE_OPEN'` |

No abbreviations (`ord_ln`), no `tbl_` prefixes, no bare `id` columns.

## 7. Configuration keys

Dotted `snake_case` paths mirroring the Tenant → Brand → Store tree:

```
store.pos.tables_enabled
store.printing.default_printer_id
store.reporting.day_cutoff_hour
brand.menu.daypart_schedules
tenant.integration.webhook_endpoints
```

## 8. Permissions

`domain.resource.action`, sharing the event taxonomy:

```
sales.order.create · sales.order_line.void_fired · sales.item.open
billing.discount.over_limit · billing.bill.void · billing.tip.adjust
cash.drawer.open_standalone · inventory.stock.adjust · admin.role.manage
```

Permission ids are contracts: add only, never remove.

## 9. Metrics and logs

**Metrics** follow Prometheus conventions: `pos_<subsystem>_<object>_<unit>` — `pos_cloud_events_ingested_total`, `pos_edge_sync_lag_seconds`, `pos_cloud_webhook_delivery_duration_seconds`. Labels are `snake_case` and **must be low-cardinality**: `store_id`, `adapter`, `event_type`, `status`. Never label by order or bill.

**Logs** are structured, reuse business field names, and never contain PII:

```json
{"level":"error","event_type":"billing.payment.captured","store_id":"…","error_status":"UNAVAILABLE"}
```

## 10. Source code

Crates use `kebab-case` (`pos-core`, `fiscal-vn`) per Cargo convention. Modules, files, functions, and variables are `snake_case`. Types, traits, and enum types are `PascalCase`. Constants are `SCREAMING_SNAKE_CASE`. Rust enum variants are `PascalCase` internally and serialise to `UPPER_SNAKE_CASE` on the wire.

## 11. Versioning

Two independent axes:

- **Product version** (SemVer, `v1.4.2`) — what is released.
- **`PROTOCOL_VERSION`** (integer, declared in `pos-proto`) — the language the cloud and edges speak.

The cloud **must** understand at least the two most recent protocol versions, because edges update in rings and may be offline for days. Protocol changes are additive; a breaking change increments `PROTOCOL_VERSION` and both versions run in parallel for at least two releases. The same discipline applies to database migrations: within a release, migrations may only add.

## 12. Deliberate deviations from Google AIP

| Deviation | Google | Here | Reason |
|---|---|---|---|
| JSON case | proto→JSON produces `lowerCamelCase` | `snake_case` | We do not use proto-JSON mapping. One rule across JSON, database, events, and permissions is worth more than matching the letter of the guide. |
| URL collection ids | `lowerCamelCase` (AIP-122) | `snake_case` | Same reason; most resources are single words anyway. |
| Money | `google.type.Money` (`units` + `nanos`) | `currency_code` + `amount_minor` | A POS never needs sub-minor precision, and `nanos` invites floating-point thinking. |
| Resource identity | a `name` field holding a path | `<resource>_id` | Simpler, predictable, and maps directly onto primary keys. |

## 13. Machine enforcement

1. **Naming linter** over the OpenAPI document, SQL migrations, and the event/permission registries: rejects camelCase, `created_at`, bare `id` columns, unlisted abbreviations, and enums missing `*_UNSPECIFIED`.
2. **Snapshots** of the public API, the event schema, the permission catalogue, and the edge's published `/api/*` routes ([`docs/snapshots/routes.txt`](snapshots/routes.txt), [ADR-0111](adr/0111-a-second-origin-may-address-the-edge.md)): any change is a visible diff in the pull request; removals are rejected.
3. **Name-parity test**: database column names must equal JSON field names for core business tables.
4. **OpenAPI is generated from code**, never hand-written, so documentation and wire format cannot drift.
