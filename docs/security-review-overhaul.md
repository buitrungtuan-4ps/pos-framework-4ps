# Security review of the overhaul's new surfaces

**Status** Accepted · **Owner** @maintainers-security · **Last reviewed** 2026-11-24
**Classification** T3 Internal — the framework's posture, not a store's data
**Relates to** [`security-review.md`](security-review.md) (the pre-production review this extends) ·
[`gate-register.md`](gate-register.md) (L5, the independent pentest, which this does not replace) ·
[`roadmap-v4.md`](roadmap-v4.md) phase 9

Roadmap v4 phase 9 asks for a security review of every surface phases 1 to 6 added. This is that
review. It follows [`security-review.md`](security-review.md): for each surface it says what the
control is and which file implements it, then lists the findings with a severity and whether each
is fixed or tracked.

**How it was made.** An AI agent made this review on 2026-11-24 at `b283e74d`, by reading the code
and its tests, and by probing the CEL parser with inputs built to overflow it. It is a desk review.
It ran no attack against a deployed system, and it is not the independent pentest that gate **L5**
requires. A person should read the findings before the pilot, and §12 lists what the pentest
should cover.

**Severity.** *Critical*: anyone on the internet can take money, data or control. *High*: an
outsider, or a role that should not, reaches personal data, money or another tenant, or can stop
the fleet. *Medium*: a control holds, but only through one layer, or only through a decision or a
deployment step nobody has made yet. *Low*: a nuisance, a bound that is implicit rather than
explicit, or a defence-in-depth gap. *Info*: no finding; the control is recorded as checked.

## 1. Summary

**No Critical or High finding.** Each new surface fails closed on its credential. Each bounds what
an untrusted caller may send. None puts personal data in an event or a log. The findings are two
Medium and twelve Low. Each Medium finding needs a person to act, or waits for the hardware of
phase 8:

| # | Surface | Finding | Severity | Status |
|---|---|---|---|---|
| G1 | Guest page | The guest page is served on the console's origin | Medium | Tracked: a deployment choice before the pilot (§2) |
| K2 | Kiosk | A kiosk's device token lives in a browser that members of the public touch | Medium | Tracked: hardware gate **P13**, phase 8's lockdown (§3) |

The Low findings are G2 to G6, K1, K3, V1 to V3, N1 and T1, listed under each surface below. G5, K1
and K3 are fixed in phase 9. The others are tracked here.

One dependency finding, **D1**, came from CI's `cargo deny` gate during the review and is fixed (§11).

## 2. The guest page `/qr/` and `/v1/qr/*`

**What it is.** A guest scans a printed table code and lands on `/qr/#<table token>`, a static page
compiled into `pos_cloud` (`crates/pos-cloud/src/qr_http/page.rs`, `guest/`). The page reads
`GET /v1/qr/menu` and sends `POST /v1/qr/orders` (`crates/pos-cloud/src/qr_http.rs`,
`qr_http/menu.rs`). [ADR-0224](adr/0224-self-order-is-a-guest-page-and-a-kiosk.md) decisions 1–3.

**The controls, checked:**

- **The token is the credential.** It is `HMAC-SHA256(secret, tenant.store.table)`. The tag is
  checked in constant time with `Mac::verify_slice`, which also refuses a truncated tag
  (`crates/pos-cloud-floor/src/qr.rs`). The table comes from the token and never from the body, so
  a guest cannot name another table. A bad token gets the same answer whatever was wrong with it.
- **The guardrails run before intake, on both routes:** QR ordering's switch (off unless the
  published version turns it on, and then `404`), the store being online, business hours, and a
  per-table sliding window. Menu reads have a counter of their own, so reading the menu never
  spends the order budget.
- **The body is bounded.** An order has 1 to 30 lines, 1 to 99 whole items a line and at most 16
  modifiers a line, and a guest's line may carry no note (`OrderLineRequest::guest_shaped`). The
  menu answer leaves out tax classes, codes and courses.
- **The page.** Every file is served with `default-src 'none'; script-src 'self'; style-src 'self';
  connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`,
  plus `no-referrer`, `nosniff` and `X-Frame-Options: DENY`. A test refuses `innerHTML`,
  `outerHTML`, `insertAdjacentHTML`, `eval(` and `document.write` in the script, so a menu name
  can only ever be text. The token travels in the URL fragment, which no request carries. The
  basket's idempotency reference is a `crypto.randomUUID()`.
- **No CORS.** `pos_cloud` sets no `Access-Control-Allow-*` header, so another site's script cannot
  read an answer.

**Findings:**

| # | Finding | Severity | Status |
|---|---|---|---|
| G1 | **The guest page shares the console's origin.** `/qr/` and `/admin/*` are served by one `pos_cloud` behind one host. The console's session cookie is `HttpOnly`, `Secure` and `SameSite=Strict`, so a script cannot read it. But a script running on the same origin can still *send* it: any script-injection bug in the guest page would be a console session for any admin who opens a guest link in the same browser. The strict CSP and the no-markup test make such a bug unlikely, which is why this is Medium and not High. It is one layer, though, on a page any stranger can open | Medium | **Tracked.** Before the pilot, serve `/qr/` and `/v1/qr/*` on a host of their own (for example `order.<domain>`) and refuse `/qr/` on the console's host. That is a Caddy site block and a printed-URL choice, so it is a deployment decision for the owner, not code |
| G2 | **A table token never expires, and one secret signs them all.** A leaked photo of a table's code lets anyone, anywhere, send orders to that table within the guardrails, until the secret changes and every code is reprinted. [ADR-0057](adr/0057-qr-ordering.md) accepted this. It is bounded by staff confirmation, which is on by default, and by the per-table rate limit and business hours. A store that turns staff confirmation off sends such an order straight to the kitchen | Low | Tracked: a per-store secret with a key id, so one store's codes can be rotated alone. Keep staff confirmation on for the pilot |
| G3 | **The menu's limit is the order limit.** Menu reads use `per_table_limit` (by default 10 a minute) on their own counter. A table of guests who each open and reload the page can be refused a menu. Anyone holding the code can also use up one table's menu and order budget, which locks out only that table | Low | Tracked: a menu read limit of its own, as a published setting |
| G4 | **The idempotency key does not include the table.** Intake is idempotent on `(tenant, store, channel, external_reference)` (`crates/pos-cloud/src/relay.rs`). A guest who sent another table's reference would get that order's id and total back, and their own lines would be dropped. The page mints the reference with `crypto.randomUUID()`, so the reference cannot be guessed | Low | Tracked: put the token's table into the QR intake's reference |
| G5 | **The per-table counters never forgot a table.** `TableRateLimiter` kept an empty entry for every table that had ever been counted. Only a validly signed token reaches it, so the map's size was bounded by the number of printed tables, not by an attacker. Still, `AGENTS.md` asks every in-memory structure for an explicit limit | Low | **Fixed in phase 9** (#1343): an entry is dropped once its window is empty, and the map has a stated ceiling |
| G6 | **The menu read carries the token in its query string,** so a reverse proxy's access log records it. `pos_cloud`'s own trace records only the path. The token is printed on the table, so this discloses nothing that is not already on display | Low | Tracked: turn off query logging on the proxy, or move the token to a header in a later API version |

## 3. The kiosk session, its scope and `/ws`

**What it is.** A till on a terminal the console made `DEVICE_CLASS_KIOSK` signs in as that
terminal's own staff record with `POST /api/session/kiosk`, with no PIN
(`crates/pos-edge/src/http/kiosk.rs`). [ADR-0224](adr/0224-self-order-is-a-guest-page-and-a-kiosk.md)
decision 4.

**The controls, checked:**

- **Sign-in.** Only a paired device that is bound to a terminal the signed `devices` node names
  `DEVICE_CLASS_KIOSK` may sign in. A binding that cannot be read is refused. Binding a device
  needs `admin.device.manage`. The kiosk's record is derived on the edge from the signed node, and
  a code-and-PIN sign-in as `KIOSK-…` is refused (tests in `crates/pos-edge/tests/kiosk.rs`).
- **Grants.** The fixed set `{sales.line.add, billing.bill.open}`
  (`pos_core::permission::KIOSK_GRANTS`) replaces whatever the package publishes for that record.
  Payment, drawer, discount, comp and reprice are refused, and tests show each refusal.
- **Scope.** `kiosk_gate` (`crates/pos-edge/src/http/kiosk_scope.rs`) is an allow-list of twelve
  routes. Each route that names an order or a line passes only if the kiosk opened that order. Any
  other route, a trailing slash or an unparsable id is refused `403 KIOSK_NOT_ALLOWED`. A test
  checks every listed route against `docs/snapshots/routes.txt`.
- **Leaving.** Leaving the kiosk is a person's ordinary PIN sign-in, with its lockout, and it needs
  `admin.kiosk.exit`. Otherwise the answer is `403 KIOSK_EXIT_NOT_ALLOWED`.
- **`/ws`.** A kiosk-bound device is refused the store's event stream (`http/ws.rs`).

**Findings:**

| # | Finding | Severity | Status |
|---|---|---|---|
| K1 | **`/ws` failed open when the binding could not be read.** The sign-in refuses a device whose binding read fails. `/ws`, though, treated a read failure as "not a kiosk" and opened the whole stream | Low (a read failure on the store's own database, from a device already paired) | **Fixed in phase 9** (#1347): `/ws` answers `503` when the binding cannot be read, on a store that has a kiosk terminal |
| K2 | **The kiosk's device token lives in a public browser.** Anyone who can open the kiosk browser's developer tools or storage can carry the token to another machine on the store's network. With it they reach the kiosk's own scope, `GET /api/pair/devices` (the paired devices' names), and code-and-PIN sign-in attempts, which the per-person lockout limits. They do not reach money, other orders or `/ws` | Medium | Tracked: phase 8's kiosk lockdown on Windows ([ADR-0201](adr/0201-the-till-runs-on-every-device-class.md)), proved on the NUC by hardware gate **P13**. Until then, a kiosk that has been tampered with is unpaired from the console |
| K3 | **A kiosk may open an order on any channel, or as a tab.** `POST /api/orders` is on the kiosk's list with its whole body: `channel` and `tab`. The till sends only the store's accepted channels. A tampered kiosk could open a tab, or an order on another channel, which changes that order's tax. It still cannot take payment, so a person sees the order | Low | **Fixed in phase 9**: a kiosk's `POST /api/orders` is refused `403 KIOSK_NOT_ALLOWED` for a tab or for a channel other than eat-in or takeaway |

## 4. The cloud gateway (ADR-0198)

**What it is.** `POST /sync/stores/{store_id}/gateway/calls`, through which a store's external
lookup goes (`crates/pos-cloud/src/gateway/`, `outbound.rs`).

**Checked; no finding.**

- **Credential.** The store's device credential or store key, with `call_gateway`, and bound to
  the store in the path (`require_store`). The endpoint must be the credential's tenant's and
  enabled, and the answer is one `404` otherwise.
- **SSRF.** Before every attempt `vet` refuses anything but `https`, credentials in the URL, and any
  address in the loopback, private, link-local, CGN, benchmarking, documentation, reserved or
  multicast ranges. It checks IPv4-mapped IPv6 and NAT64 too (`webhook/ssrf.rs`, with its tests).
  The dial goes only to the addresses vetted, with TLS verified against the hostname, so DNS
  rebinding has no window. No redirect is followed, and the answer is capped at 256 KiB.
- **Values.** A store's values are checked against the endpoint's declared fields, at most 1,024
  characters each, and put into the query by `url::Url::query_pairs_mut`, never by string
  concatenation. Only declared answer fields come back.
- **Budgets and retries.** There is a per-store and a per-endpoint rate limit, a circuit breaker
  per tenant, and at most two retries, only for an endpoint declared idempotent.
- **Secrets.** Header secrets are sealed under the tenant's versioned key
  ([ADR-0217](adr/0217-secrets-are-sealed-under-versioned-tenant-keys.md)). `OutboundRequest`'s
  `Debug` prints neither a header nor the body, and nothing logs a value.

*Info:* the replay cache is keyed by `(tenant, store, idempotency key)` without the endpoint, so a
store that reuses a key across two endpoints gets the first answer back. It is the store's own
data, so this is not a finding, but it is worth knowing when writing a till act.

## 5. Webhooks and their filters

**Checked; no finding.** A tenant's webhook URL is vetted when it is saved, and again before each
batch is delivered. The transport dials only the vetted addresses (`webhook/transport.rs`). Each
delivery is HMAC-signed, has a stable `pos-delivery-id`, and is stopped by a circuit breaker on a
dead endpoint. The event-type filter (`WebhookEndpoint::sends`) chooses which events are sent. An
empty filter sends all of them, as before. The payload is the event log, which holds no personal
data by construction (`crates/pos-proto/src/pii.rs`).

## 6. Delivery-vendor intake and the public API

**What it is.** `POST /connectors/{tenant_id}/{connection_id}/orders` (`crates/pos-cloud/src/connectors/route.rs`)
and `POST /v1/orders` (`orders.rs`). [ADR-0215](adr/0215-a-delivery-order-names-its-vendor.md).

**The controls, checked:** the connection must be enabled, store-scoped and of a known provider,
and the answer is one `404` otherwise. The body is at most 64 KiB, and each connection may send 20
a second. The vendor's signature is checked over the raw body in constant time, before the body is
parsed. A recipient is kept apart as a subject record under the store's retention, and the order
carries only its `subject_id`. A refusal logs the connection id and the reason, never the body. On
`/v1/orders` the key's scope is checked, the store is bound to the key's tenant (an unknown store
and another tenant's store get the same `404`), and each tenant has a budget.

| # | Finding | Severity | Status |
|---|---|---|---|
| V1 | **The per-connection limit is charged before the signature is checked.** That is the right order for a limiter. But anyone who knows a connection's URL can spend its 20 a second and delay the real vendor's orders | Low | Tracked: a second limit charged only after the signature verifies |
| V2 | **The demo connector signs the body alone, with no timestamp.** A captured webhook can be replayed. Intake is idempotent on the vendor's reference, so a replay creates nothing | Low | Tracked: a real connector checks the vendor's timestamp or nonce wherever the vendor sends one. This is a line for the connector checklist in [ADR-0083](adr/0083-integration-doctrine.md) |
| V3 | **The recipient rests in the cell.** Name, phone and address are kept in the cell's `subjects` table, so they cross a border whenever the cell runs outside the store's country. For Vietnam the owner decided on 2026-10-14 that the cell is hosted in Vietnam (gate **L6**), so this holds only if the pilot's cell is actually deployed there. For Japan and India it is the open B10.4 decision | Low | Tracked: deploy the pilot's cell in Vietnam; B10.4 ([`data-residency.md`](data-residency.md#b104-the-decision-the-owner-owes)) and gate **L3** for the other countries |

## 7. The package push and the NATS wake (ADR-0195)

**The controls, checked:** a wake carries only a `config_version_id`, and the store then pulls
over the authenticated `/sync` rail it already uses. The pull's tenant check and last-known-good
apply stay the only way configuration reaches a store. Wake-driven pulls run at most one at a time
and at most one a second, so a flood of wakes costs one `up_to_date` answer a second. Every CEL
condition in a package is compiled under §8's limits before the version is applied.

| # | Finding | Severity | Status |
|---|---|---|---|
| N1 | **One fleet token on the bus.** Until per-store mTLS ([ADR-0089](adr/0089-edge-event-bus-transport.md)), any box can subscribe to every store's wake subject, which shows when a store publishes, and can forge a wake, which costs that store one pull | Low | Tracked with gate **H12**, until per-store mTLS lands as [ADR-0195](adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md) decision 5 describes |

## 8. CEL evaluation limits (ADR-0193, ADR-0202)

**Checked; no finding.** `pos_rules::Rules::compile` refuses anything over 1,024 characters before
it parses. It then counts bracket depth in the text, outside string literals, and refuses anything
deeper than 32 before the parser recurses. This matters because the unoptimised parser overflows a
2 MiB stack at about 16 brackets, and an overflow aborts the process. After parsing, a walk refuses
more than 256 nodes, a depth over 32, the five loop macros, any function outside the 21 allowed,
`double`, struct literals and unknown variables. A panic inside the evaluator is caught and becomes
`RuleError::Panicked`.

This review also probed nesting that uses no bracket, on the default 2 MiB test stack:
1,000 `!` in a row, 1,000 unary minus signs, 70 nested ternaries, 120 chained `||` and 400 member
selections. Each was refused or compiled without overflowing. Those probes are now regression
tests in `crates/pos-rules/src/tests.rs`.

## 9. Custom data: keeping personal values apart (ADR-0194, ADR-0206)

**Checked; no finding.** A field declared personal is stored in the edge's `subjects` table and
never in an event, a log or the cloud. Each value expires on its field's own period. Reading one
needs `data.personal.read`. An erasure travels on the `erasures` node, and the store confirms it
with an event that carries only the id. `crates/pos-edge/tests/personal_values.rs` proves that a
personal value reaches no event and no log. Gate **L8** (full-disk encryption on a box that holds
personal values) stays a human gate.

## 10. A bar tab's guest label (ADR-0221)

**Checked.** The label is off by default (`tabs.guest_label`), at most 32 characters with no
control characters, and kept in the edge's subject store under the order. It is masked when its
retention ends, and never written to an event, a log, a ticket or the cloud
(`crates/pos-edge/tests/tab_label.rs`). Writing one needs `sales.line.add`. A kiosk cannot reach
it, because the route is not on its list.

| # | Finding | Severity | Status |
|---|---|---|---|
| T1 | **Any signed-in person on any till reads every open tab's label** in `GET /api/tabs`. That is the purpose of the label, which is to find a tab again. It does mean that the store's DPIA entry covers every member of staff as a reader | Low | Tracked: the DPIA note in [`data-residency.md`](data-residency.md#a-bar-tabs-guest-label-dpia-note) says so; no code change |

## 11. Dependencies

`cargo deny check advisories` runs on every pull request and nightly
([`security-review.md`](security-review.md) §6). During this review it went red on every open pull
request:

| # | Finding | Severity | Status |
|---|---|---|---|
| D1 | **`url` 2.5.8 has a RustSec advisory, RUSTSEC-2026-0592.** The `url` crate parses every address the gateway and webhook SSRF vet checks (`webhook/ssrf.rs`, §4 and §5), and the gateway's query building. Its severity is the one the advisory gives; this review did not assess it further | As the advisory rates it | **Fixed in #1344**, which moves `Cargo.lock` to `url` 2.5.9 |

The gate did its job: no change merged while the advisory was open.

## 12. What the pentest (gate L5) should add to its scope

These go on top of [`security-review.md`](security-review.md) §9:

1. **The guest page.** Try to inject script through every menu field the console lets HQ write. Try
   to replay a table's token on another store. Try to walk the guardrails rather than the
   signature. If G1 is still open, show what a script on `/qr/` can do on `/admin`.
2. **The kiosk.** With a kiosk's device token, try each route outside the allow-list, each order
   the kiosk did not open, `/ws`, and leaving the kiosk without `admin.kiosk.exit`.
3. **The gateway and webhooks.** Try SSRF through every way of writing an address: decimal, octal,
   IPv6 zone ids, NAT64, a redirect, and DNS that answers differently on its second lookup.
4. **Connector intake.** Try forged signatures, oversized bodies and a connection of another
   tenant.

## 13. Keeping this page honest

A finding moves to *Fixed* only with the pull request that fixes it and the test that holds it. A
new surface after phase 9 adds a section here, or its own review that this page links to.
