# Integrate with the API

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-04

For a **third party** — a delivery marketplace, an ERP, a loyalty app, an analytics tool — connecting
to a Pizza 4P's cloud. Three parts, in the order you will need them: get a credential and make a
call, learn what the surface offers, then receive events as they happen.

The other guides in this folder are for someone running the software. This one is for someone
calling it from outside.

> Every path below is on the cloud only. Stores never accept inbound connections
> ([ADR-0003](../adr/0003-cattle-not-pets.md)) — a store dials **out** to the cloud and nothing
> dials in, so there is no store endpoint to integrate with. Orders you place reach the shop because
> the store pulls them ([ADR-0061](../adr/0061-order-relay.md)); that is why placing one is not the
> same as the kitchen having it, and why [Part 2](#part-2--the-api-tour) is specific about what a
> `201` means.

---

## Part 1 — Authentication

### Getting a key

Keys are minted by an operator in the back-office console, not self-served: **Settings → API keys →
Issue**. Ask for the **scopes** you need (below) and you are handed a token **once**. It is shown a
single time and stored only as a hash, so if it is lost it is replaced, never recovered.

A token looks like this, and the three parts matter:

```
pos_01J8XYZ0000000000000000000_kQ8v…
└┬─┘ └────────────┬───────────┘ └─┬─┘
 │                │               └── the secret; compared in constant time, never logged
 │                └── the key's public id (a ULID) — quote this in a support request
 └── the fixed prefix, so a leaked token is recognisable in a log or a repository scan
```

Send it as a bearer token on every call:

```
Authorization: Bearer pos_01J8XYZ0000000000000000000_kQ8v…
```

### Scopes

Deny by default: a key authorises **nothing** outside the scopes it was issued with. Two are meant
for an integrator:

| Scope | What it unlocks |
|---|---|
| `place_orders` | `POST /v1/orders` and `GET /v1/orders` — the public order intake |
| `read_rollups` | `GET /v1/stores/{store_id}/rollups/daily` — per-store daily activity |

The remaining scopes (`read_config`, `relay_orders`, `manage_devices`) are **store** credentials — a
shop's own box holds them to pull its configuration and its queued orders. An integrator has no use
for them and should not be given them.

Ask only for what you use. A key with `place_orders` alone cannot read a single figure, which is the
answer you want when a laptop goes missing.

### Tenancy is the boundary

A key belongs to exactly one **tenant** (a brand or company), and every request is confined to it. A
store id from another tenant does not come back as "forbidden" — it comes back as **`404`**, the same
as an id that does not exist, deliberately, so nobody can map another company's estate by probing.

### What a refusal looks like

One shape everywhere ([AIP-193](https://google.aip.dev/193), `docs/naming-and-api.md` §4):

```json
{
  "error": {
    "code": 403,
    "status": "PERMISSION_DENIED",
    "message": "this key was not granted place_orders",
    "details": []
  }
}
```

Branch on `status`, not on `message` — messages are for humans and may be reworded. Treat a `status`
you do not recognise as its `code` implies: it parses openly, so a newer server adding a status will
not break a client that predates it.

The ones you will meet:

| `status` | HTTP | What to do |
|---|---|---|
| `UNAUTHENTICATED` | `401` | The token is missing, malformed, revoked or expired. Do not retry with it. |
| `PERMISSION_DENIED` | `403` | The key lacks a scope. Ask the operator to reissue; retrying will not help. |
| `INVALID_ARGUMENT` | `400` | A field is wrong. `details` names it. Fix and resend. |
| `NOT_FOUND` | `404` | No such resource **within your tenant**. |
| `FAILED_PRECONDITION` | `409` | The store refused it (a closed shift, an unsellable item). |
| `RESOURCE_EXHAUSTED` | `429` | You are over a rate limit — see below. |
| `UNAVAILABLE` | `503` | Transient. Retry with backoff; for orders, see the look-up in Part 2. |

### Rate limits

`/v1/orders` is budgeted **per tenant** over a sliding one-minute window (300 calls by default, a
deployment can change it). Both the submit and the look-up spend from the same budget, because the
look-up is the call you make when a submit times out.

A refusal is a `429` with a **`Retry-After`** in whole seconds. Honour it: the window is sliding, so
retrying earlier neither succeeds nor punishes you further — it simply fails again.

```
HTTP/1.1 429 Too Many Requests
retry-after: 12
```

If you are hitting the limit in normal operation, ask the operator to raise it rather than
engineering around it. The default is set to bound a runaway retry loop, not to shape real traffic.

---

## Part 2 — The API tour

The machine-readable contract is served live at **`GET /v1/openapi.json`** and is the authority; this
section is the map.

Conventions worth knowing before the first call:

- **`snake_case`** everywhere on the wire — fields, query params, enum tokens.
- **Ids are ULIDs**, 26 characters, sortable by creation time.
- **Money is an integer plus a currency**, never a float: `{"currency_code":"VND","amount_minor":150000}`.
  `amount_minor` is in the currency's minor unit — đồng for VND (no subunit), cents for USD.
- **Enums are `UPPER_SNAKE_CASE`** with an `*_UNSPECIFIED` zero value, and you **must** treat an
  unknown value as unspecified rather than failing. That is what makes adding one non-breaking.
- **Times are RFC 3339 UTC** and end in `_time`. `business_date` is different — it is the store's
  trading day, derived from its own cut-off hour, so a sale at 01:00 can belong to yesterday.

### Placing an order — `POST /v1/orders`

Scope: `place_orders`.

```json
{
  "external_reference": "GRAB-8891422",
  "sales_channel": "SALES_CHANNEL_DELIVERY",
  "store_id": "01J8ZZ0000000000000000STOR",
  "lines": [
    { "menu_item_id": "01J8ZZ0000000000000000ITEM", "quantity_milli": 1000 },
    { "menu_item_id": "01J8ZZ0000000000000000ITEM", "quantity_milli": 2000,
      "modifier_menu_item_ids": ["01J8ZZ00000000000000000MOD"] }
  ],
  "placed_at_ms": 1767225600000
}
```

- **`external_reference` is your idempotency key.** Send the same one twice and you get the same
  order back with `"created": false` — never a duplicate. Use your own order number; that is exactly
  what it is for.
- **`quantity_milli`** is thousandths, so `1000` is one and `500` is a half (split items are real).
- **`quoted_unit_price`** is optional and is a *quote*, not an instruction. The store prices from its
  own published menu; if your quote disagreed, the response comes back with `"repriced": true` and
  the store's figure. Show the store's figure to the guest.
- **`table_id`** is for a dine-in or QR order. A delivery or counter order is tableless by design.
- **A line's `note` is optional, and it is personal data.** It goes to the kitchen and appears on the
  ticket, but the store's **event log records only that a note existed**, not its text — so it never
  reaches the cloud's reporting, the webhook stream, or an export
  ([ADR-0076](../adr/0076-subject-request-tooling.md)). Send a preparation instruction ("no chilli"),
  not a name, a phone number or an address. Anything identifying a person belongs in your own system
  under your own lawful basis; the Vietnam PDPD and GDPR obligations for it do not transfer by
  putting it in this field. **Never** put a guest's contact details here to get them onto a ticket.

The response:

```json
{
  "order_id": "01J8ZZ00000000000000000ORD",
  "created": true,
  "queue_number": 42,
  "total": { "currency_code": "VND", "amount_minor": 465000 },
  "repriced": false,
  "awaiting_staff_confirmation": false
}
```

**`201` means the store accepted it, not that the kitchen has started.** `awaiting_staff_confirmation`
is `true` when the shop's policy is that a human waves an inbound order through — the order is real
and queued, and it is not being made yet.

**On a `503`, do not resubmit blindly.** The order may have landed. Use the look-up:

```
GET /v1/orders?store_id=…&sales_channel=SALES_CHANNEL_DELIVERY&external_reference=GRAB-8891422
```

It answers with the same body as a submit if that reference already produced an order, and `404` if
it did not. This is the whole reason `external_reference` exists — resubmitting with the same
reference is also safe, but the look-up tells you what happened without placing anything.

### Reading activity — `GET /v1/stores/{store_id}/rollups/daily`

Scope: `read_rollups`. Windowed with `?from=&to=&limit=`, dates as `YYYY-MM-DD` on the store's
**business date**, so a day here is the store's trading day and matches its own Z report.

Rollups are computed from the store's event stream after it syncs. A store that has been offline
reports nothing for those days until it reconnects and drains — the figures are eventually complete,
not live. For anything that must be immediate, use webhooks (Part 3).

### QR ordering — `POST /v1/qr/orders`

A guest-facing endpoint for a table QR code, authenticated by a signed table token rather than an
API key, and subject to the store's own guardrails (what may be ordered, whether staff confirm).
Documented in [ADR-0057](../adr/0057-qr-ordering.md); it is not a general integration path.

### What is deliberately not here

- **No public event feed.** There is no `GET /v1/events`. Events reach you by webhook (Part 3).
- **No `pos-api-version` pin.** It existed as a header nothing read, which is worse than none, so it
  was removed. `/v1` grows additively — new fields, new enum values — and the tolerance rules above
  are how you stay compatible.
- **No writes beyond the order intake.** Catalogue, prices, staff and configuration are authored in
  the console and published to stores; they are not an API an integrator writes to.

---

## Part 3 — Webhooks

### How delivery works

An operator registers your endpoint in the console (**Settings → Webhooks**) with an `https://` URL,
and is shown a **signing secret** once. From then on the cloud pushes that store's events to you.

Three properties to build against
([ADR-0032](../adr/0032-webhooks.md)):

1. **A delivery is a page of events, not one event.** The body is a JSON array of event envelopes.
2. **It is re-sent unchanged until you accept it.** Reply `2xx` and the cursor advances; reply
   anything else, or time out, and the *same page* arrives again. There is no partial credit — so if
   you accept a page, you own every event in it.
3. **Repeated failure disables the endpoint.** A circuit breaker backs off and eventually stops
   delivering, and an operator re-enables it from the console. The cursor is kept, so it resumes
   where it fell behind rather than replaying your whole history.

Because of (2), **your receiver must be idempotent.** Dedupe on each event's own `event_id` inside
the body — every envelope carries one, and it is a ULID, so it is also your ordering key.

### The two headers

| Header | Value |
|---|---|
| `pos-signature` | `v1=` followed by the HMAC-SHA256, lowercase hex |
| `pos-signature-time` | The instant it was signed, Unix **seconds** |

There is no `X-` prefix, and there is no per-event or per-delivery id header. If you built against
`X-Pos-Webhook-Signature`, that name is gone — same algorithm, same secret, new spelling.

### Verifying a delivery

The signed bytes are the timestamp, a literal `.`, then **the raw body**:

```
signed_payload = "{pos-signature-time}." || raw_request_body
expected       = "v1=" || hex(HMAC_SHA256(signing_secret, signed_payload))
```

Three rules, and skipping any one of them defeats the point:

1. **Sign the raw bytes**, before any JSON parse or re-serialise. Re-encoding changes the bytes and
   every signature will fail.
2. **Reject a timestamp more than 5 minutes from your own clock**, in either direction. This is what
   closes the replay window; the signature alone does not, because a captured delivery stays valid
   forever without it.
3. **Compare in constant time.** A byte-by-byte early exit leaks the expected value one character at
   a time.

A complete receiver, in Python:

```python
import hashlib, hmac, time
from flask import Flask, request

SECRET = b"the secret the console showed once"
TOLERANCE_SECONDS = 300

app = Flask(__name__)

@app.post("/pos/webhook")
def receive():
    signature = request.headers.get("pos-signature", "")
    sent_at = request.headers.get("pos-signature-time", "")
    try:
        skew = abs(int(time.time()) - int(sent_at))
    except ValueError:
        return "", 400
    if skew > TOLERANCE_SECONDS:
        return "", 400                      # replay window: refuse before hashing

    body = request.get_data()               # raw bytes, not request.json
    expected = "v1=" + hmac.new(
        SECRET, sent_at.encode() + b"." + body, hashlib.sha256
    ).hexdigest()
    if not hmac.compare_digest(expected, signature):
        return "", 401                      # constant time

    for event in request.get_json():        # safe to parse now
        handle_once(event["event_id"], event)

    return "", 204                          # only now does the cursor advance
```

Return `2xx` **after** you have durably recorded the page — not before. A `204` you send and then
crash on is a page you will never see again.

### What is in an event

Every envelope carries its `event_id`, `event_type` (`sales.order.settled`,
`billing.bill.settled`, …), the `store_id`, the `business_date`, and a versioned payload. The full
catalogue is `docs/snapshots/events.txt`, and payload shapes are versioned **per event type**, so a
field can be added to one event without anything else changing.

**Events never carry personal data.** A guest's name, phone or note is not in the payload — a field
that would identify someone is stored separately and referenced by a `subject_id`
([ADR-0076](../adr/0076-subject-request-tooling.md)). If your integration needs personal data, that
is a separate, consented data flow with its own lawful basis, not something to read off this stream.

---

## Checklist before you go live

- [ ] The key is stored as a secret, not in source, and its scopes are the minimum you use.
- [ ] `external_reference` is your own order id, and you never reuse one for a different order.
- [ ] A `503` on submit triggers a **look-up**, not a blind resubmit.
- [ ] A `429` is honoured for the whole `Retry-After`.
- [ ] `status` drives your error branching; an unknown one degrades rather than crashing.
- [ ] An unknown enum value is treated as unspecified, and an unknown field is ignored.
- [ ] The webhook receiver verifies the signature over **raw bytes**, checks the ±5-minute window,
      and compares in constant time.
- [ ] The receiver dedupes on `event_id` and returns `2xx` only after a durable write.
- [ ] You have tested what your integration does while the store is **offline** — orders queue, and
      rollups for those days are simply absent until it reconnects.
