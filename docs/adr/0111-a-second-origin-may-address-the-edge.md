# ADR-0111 — A second origin may address the edge, from a list the cloud publishes

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-06
· Serves [ADR-0110](0110-edge-placement-is-a-deployment-axis.md)'s hosted placements
· Extends [ADR-0030](0030-pairing-and-offline-auth.md)'s pairing and
[ADR-0084](0084-device-authentication.md)'s bearer gate without moving either
· Sources its list from [ADR-0004](0004-cloud-owned-configuration.md) over
[ADR-0033](0033-config-tree.md)'s layers
· Relates to [ADR-0018](0018-http-websocket-stack.md),
[ADR-0024](0024-protocol-version-negotiation.md),
[ADR-0086](0086-edge-keyvault-and-activation.md),
[ADR-0091](0091-durable-edge-auth-state.md)

This is the second of the five records ADR-0110 names. **ADR-0112** (the print agent), **ADR-0113**
(the host tier and the console's Start button) and **ADR-0114** (region as a required, recorded
attribute) are still reserved numbers with no files, so they are named in plain text and not linked
— `xtask links` fails a build on a link that does not resolve.

## The problem

### The client's opening line is a design decision, and it has become a constraint

[`ui/src/api/client.ts`](../../ui/src/api/client.ts) states it in its first sentence:

```ts
// The typed HTTP client for the edge's domain routes. Every call is one `fetch` to the same origin
// that served the app, so it works on the store LAN with no configuration.
```

That is not a comment about a comment. It is the code: `request()` calls `fetch(path)` with a
root-relative path and nothing else, and [`live.ts`](../../ui/src/api/live.ts) builds its socket URL
from `window.location.host`. There is no base-URL setting because there has never been a second
place the app could be served from.

The other half is the server. There is **no CORS layer anywhere** — not in `pos-edge`, not in
`pos-cloud`. `crates/pos-edge/Cargo.toml` takes `tower-http` with `features = ["trace"]` and nothing
else. No handler reads an `Origin` header, on `/api/*` or on `/ws`. That is exactly right for a
browser served by the edge over a shop LAN, and it means the edge answers a cross-origin caller with
a response the browser then refuses to hand to the page.

So the same-origin assumption is written twice, once on each side, and it holds three things shut at
once.

### Three different callers need the same missing thing

**A native app.** Whatever shell ships — Tauri, or something else if the spike goes badly — the
webview's origin is the shell's own. It is not the edge's. Every `fetch` it makes is cross-origin
from the first one, including the one that pairs it.

**A hosted placement.** [ADR-0110](0110-edge-placement-is-a-deployment-axis.md) puts `pos_edge` on a
VPS or a platform region. A browser on the counter reaches it over a WAN by hostname, which is fine
— that path is still same-origin, because the app is still served by the edge. But the record's own
closing bullet is the honest statement of what is missing: `EdgeConfig::advertised_ip` has nothing
to mean, and the client's "so it works on the store LAN with no configuration" keeps its first
clause and loses its second. A device that is not on the edge's LAN has no supported way to be told
where the edge is.

**A second front-end.** A kitchen display built by an operator, a self-order kiosk, a manager's
phone app: each is its own origin and each is refused today.

### The token lives somewhere a browser can throw away

The device token is kept in `localStorage` under `pos-edge.device-token`, with every access wrapped
in `try`/`catch` because a private window throws. The comment is candid about the consequence:

```ts
// A device that cannot persist its token re-pairs each session; that is degraded, not broken.
```

On a dedicated till in a shop that is a fair trade. On a phone, or on a shared tablet whose browser
data an operator clears to fix something unrelated, it means the device is silently un-paired and
somebody has to walk to the box for a six-digit code. And a native app that keeps a long-lived
credential in web storage has thrown away the one thing a native shell is for.

### The pairing URL names a building

[`pairing_url`](../../crates/pos-edge/src/pairing.rs) has the constraint in its signature:

```rust
pub fn pairing_url(host: IpAddr, port: u16, code: &Code) -> String {
    format!("http://{host}:{port}/pair?code={}", code.as_str())
}
```

It takes an `IpAddr`. It cannot carry a hostname, and it cannot emit `https`. Both were deliberate:
[ADR-0030](0030-pairing-and-offline-auth.md) chose a raw-IP `http` URL because Chrome on Android
does not resolve mDNS and a DHCP reservation pins the address, and it is the path that needs no name
resolution at all. Every word of that is still true on a shop LAN. None of it describes a placement
that is reachable only over the internet, where there is no LAN IP to print, and where a bearer
token crossing a WAN in clear text is not a thing anyone should ship.

### Two release trains, and nothing tells anyone they have diverged

The edge ships through OTA rings. An app in an app store ships when a review board says so, onto
devices that update when their owners let them. Those two clocks do not agree, and at 500+ stores
they will not even agree within one brand.

Today nothing would report the disagreement. `/healthz` reports `version` and `protocol_version`,
but the app has no reason to call it and no rule about what to do with the answer. So the first
symptom of drift is a `404` on a route one side has and the other does not, surfacing to a cashier
as `ApiError` with whatever `statusText` the response carried. That is a support call that starts
with "it just stopped working" and ends, hours later, with somebody comparing version strings.

## The decision

**The edge answers a cross-origin request only from an origin the cloud published for that store,
authenticated by the bearer token it already requires, with no cookie and no credentialed CORS mode
— and same-origin stays permitted unconditionally, so a store that is published no list behaves
exactly as it does today.**

### The allow-list is a config node, because who may address a store is fleet configuration

`AGENTS.md` §1: *"All configuration lives in the cloud and is pushed down. A store never owns its own
settings."* An origin list a store types into a file is a store owning a security setting, which is
the thing [ADR-0004](0004-cloud-owned-configuration.md) exists to prevent — it drifts, it is not
auditable, and it does not survive the machine being replaced.

So the list is a new config-tree node, `origins`, authored on the **Brand** layer
([ADR-0033](0033-config-tree.md)'s Tenant → Brand → Store → Device), published like every other node,
and applied by `apply_document` in
[`config_client.rs`](../../crates/pos-edge/src/config_client.rs) alongside `floor`, `stations`,
`tender` and the rest. It is authored, not derived — unlike `lease`
([ADR-0108](0108-the-lease-generation-is-authority.md)), which is a table the cloud projects — because
"which shells may talk to my tills" is a decision a person makes and should be able to see, diff and
roll back.

It is its own node rather than a field on `capabilities`, for the reason ADR-0108 kept `lease` out of
`device_ota`: different author, different lifecycle. `capabilities` is edited by whoever tunes how a
store trades ([ADR-0071](0071-config-without-json.md)); the origin list is edited when a shell ships.
Merging them lets a capability edit change who may address the edge.

Four rules make the node safe, and each closes a specific hole:

- **The list is additional to same-origin, never a replacement.** A request whose `Origin` matches
  the origin that served the page is allowed with no list at all. Without this rule, a store with no
  `origins` node published would refuse its own UI, and the never-blank contract would turn a
  malformed publish into a dark shop.
- **No wildcards.** `https://*.example.com` is refused. An exact origin is a string comparison; a
  wildcard makes it a parser, and a parser is where `https://evil.test#.example.com` gets in.
- **`null` is refused.** A sandboxed iframe, a `file://` page and several redirect shapes all send
  `Origin: null`. Allow-listing `null` allow-lists all of them at once and names none of them.
- **The list is bounded at eight entries, and a document with more is refused whole.** `AGENTS.md` §2
  forbids an unbounded in-memory structure, and this one is read on the front of every request. Eight
  is one origin per shipped shell with headroom; a fleet that needs more has a different problem than
  a bigger array.

The node follows the config tree's never-blank rule exactly as `lease` does: a document that omits
`origins`, or carries one that does not parse, leaves the previously-applied list alone and logs.
A malformed publish must not silently open the edge to nobody — or to everybody.

### Twenty-five routes are reachable cross-origin. Two are not, and neither is the probe or the app

The edge serves twenty-seven `/api/*` routes (two of them, `/api/activate` and `/api/activation`,
only on a box provisioned for a cloud) plus `/ws` and `/healthz`.

**Covered — twenty-five.** Everything a till does: the eighteen guarded domain routes, the three
session routes, `/api/orders/open`, `/api/pair/devices`, `/api/pair/revoke`, and `/api/pair`. These
are the surface the app is for. Twenty-two of them already sit behind two gates —
`require_paired_device` then `require_signed_in`
([`http/auth.rs`](../../crates/pos-edge/src/http/auth.rs)) — so a cross-origin caller reaching them
still needs a device token *and* an employee signed in on that device. CORS adds a third
condition to routes that already had two.

**Not covered — `/api/activate` and `/api/activation`.** Activation is the box's own setup: an
operator types a code from the store's setup sheet into the `/setup` screen the box serves
([ADR-0086](0086-edge-keyvault-and-activation.md)), and the credential lands in that box's OS
keyring. There is no cross-origin actor in that story, and a route that exchanges a code for a
long-lived machine credential should not be reachable from a page. These stay same-origin-only, and
a hosted placement that must be activated is activated from the console-driven flow ADR-0113 builds,
not from a till.

**Not covered — `/healthz`.** It is what a service manager polls. A service manager is not a browser
and never sends `Origin`, so CORS headers on it would serve nobody, and putting it on the list would
publish `store_id` to a page for no gain.

**Not covered — the asset fallback** (`http::assets::serve`). Cross-origin `fetch` of `index.html` is
not a use case; a shell that wants the app bundles it.

### Pairing is on the list, and the reason it is safe is not the allow-list

This is the decision worth arguing, because the instinct is to keep the unauthenticated door
same-origin-only.

That instinct fails on the native app. If `POST /api/pair` is same-origin-only, the app cannot pair
at all; the best it could do is open a webview on the edge's own origin, redeem a code there, and
inherit the token from *that* origin's storage. Which puts the credential back in web storage on the
edge's origin, defeating the keychain decision three sub-headings down, and adds a second pairing
path that only one kind of client uses. ADR-0110 is explicit that the moment a path forks on
placement the framework has two systems and tests one; a second pairing path is that fork one level
lower.

The alternative shape is cloud-brokered pairing, and [ADR-0030](0030-pairing-and-offline-auth.md)
rejected it outright as option 2 — *"Pairing a new tablet during an internet outage is exactly when a
store needs it to work."* That refusal is about `in-store`, where it still binds absolutely. It would
be possible to argue that a hosted placement has no offline pairing to protect, so it may broker
through the cloud. It is possible and it is wrong, for the same reason: one pairing flow, tested by
everybody, beats two flows of which the rarely-run one is the one you need during an incident.

**So `/api/pair` is reachable from an allow-listed origin.** What must be said plainly alongside that
is what the allow-list does *not* buy:

> **CORS is not authentication.** It is a rule the browser enforces on a page's behalf. `curl` sends
> whatever `Origin` it likes and reads the response regardless. An allow-list stops a random web page
> from driving a paired user's device; it stops no script anywhere.

And the widening people will worry about is not caused by this record. In-store, an attacker
guessing pairing codes must be on the shop LAN. Hosted, the edge is on the internet and anyone can
reach it. That change comes from ADR-0110 moving the process, and it is there whether or not `/api/pair`
answers a cross-origin caller. What actually defends the endpoint is the S4 budget and, for a hosted
placement, the proxy in front of it — settled at the end of this section.

### The token is a bearer, so `Access-Control-Allow-Credentials` is never set

Stated flatly, because this is the hole:

- **`Access-Control-Allow-Credentials` is not sent. Not `true`, not `false` — the header is absent.**
- **No cookie is involved anywhere on the edge.** There is no `Set-Cookie` in `pos-edge` today and
  none is added. The token travels in `Authorization: Bearer <token>`, built by `authHeaders()` in
  the typed client, and on `/ws` in the subprotocol list because the browser `WebSocket` API cannot
  set a header ([`ws.rs`](../../crates/pos-edge/src/http/ws.rs)).
- **`Access-Control-Allow-Origin` echoes the single matched origin.** Never `*`, and never a list.

The cookie question is not a preference. A cookie is an *ambient* credential: the browser attaches it
to a qualifying request whether or not the code that made the request knew it existed. Turn on
`Allow-Credentials: true` with a cookie session and every allow-listed origin can drive the till with
the operator's own authority, and so can any script that achieves cross-site scripting on any one of
those origins, and so can a cross-site form post — the till is one CSRF away from settling a bill.
A bearer token in a header has none of that: a cross-site form cannot set `Authorization`, so there
is no CSRF surface to defend, and the code that sends the credential is the code that read it.

`*` is refused for a narrower reason. Twenty-two of the twenty-five covered routes are protected by
the two auth gates and would survive it. `/api/pair` is not — it is unauthenticated by design,
because it is how a device obtains the credential. `Allow-Origin: *` hands every page on the internet
a redemption attempt against every reachable edge, which means any page can spend a store's S4 budget
from every browser that loads it and keep pairing shut for as long as it has visitors. An allow-list
of one to eight strings costs nothing and removes that entirely.

### The preflight is the real cost, and `Vary: Origin` is the real bug

`Authorization` is not a CORS-safelisted request header, so **every cross-origin `/api/*` call is
preceded by an `OPTIONS` preflight**. Three consequences, all of them things that break if they are
got wrong:

**The CORS layer sits outside both auth middlewares.** A preflight carries no `Authorization` header,
by specification. If the layer is applied inside `require_paired_device`, every preflight is answered
`401` and every cross-origin call fails — and the failure reads to an operator as "pairing is
broken", which is the worst possible mislabelling of a routing mistake. The layer short-circuits
`OPTIONS` before the gates run.

**The preflight response allows `authorization` and `content-type`, methods `GET`, `POST` and
`OPTIONS`, and is cached.** Those are the only header and method shapes the client sends. Caching it
matters more than it looks: without `Access-Control-Max-Age`, a hosted placement pays two WAN round
trips per call instead of one, and the `/ws` fan-out's under-50 ms budget
([ADR-0018](0018-http-websocket-stack.md)) was measured across a shop, not across a WAN, so the
latency headroom is already gone.

**Every response that varies on the request's origin carries `Vary: Origin`.** This is the classic
defect in a hand-rolled CORS layer: an intermediary caches a response containing
`Access-Control-Allow-Origin: https://a.example` and serves it to a request from
`https://b.example`, which either breaks b or — with a permissive cache in front of a
credential-bearing response — leaks across origins. `Vary: Origin` is not a nicety.

The implementation is `tower_http::cors::CorsLayer`, which means enabling the `cors` feature on a
dependency `pos-edge` already carries rather than adding a crate to the graph. `AGENTS.md` §2 requires
a merged ADR before a dependency changes; this is it.

### `/ws` is not a CORS problem, it is an `Origin` problem

WebSocket handshakes are exempt from the same-origin policy. A browser will open a socket to any host
without a preflight and without asking anyone, which is why "we added CORS" is not an answer for
`/ws`.

So `/ws` checks the `Origin` header on the upgrade against the same list, plus the serving origin:

- **`Origin` present and not on the list** — refused before the upgrade.
- **`Origin` absent** — allowed. A native client or a non-browser consumer sends none, and
  [`ws.rs`](../../crates/pos-edge/src/http/ws.rs) already documents that case: the endpoint accepts
  a client that offers no subprotocol and authenticates with the `Authorization` header instead.

Be honest about what this buys. `require_paired_device_ws` is still the authority, and a page on an
unlisted origin cannot read the token out of another origin's storage, so it cannot offer the
subprotocol and cannot connect anyway. The `Origin` check is worth one string comparison for one
reason: `/ws` is the single endpoint a browser will open cross-origin unasked, so on the day a token
does leak — a screenshot, a support paste, a shared kiosk — it is the endpoint with no other barrier
in front of it. It is defence in depth, and it is described as that rather than dressed up.

### The base URL defaults to the empty string, so the browser path is byte-identical

`request()` gains a base: `fetch(base() + path)`, where the default value of `base()` is `""`.

**Not `window.location.origin`.** That would produce an absolute URL where a root-relative one is
sent today — a different request string, a different set of things that can go wrong with it, and a
change to the one path that is currently working in every shop. The empty string emits the identical
bytes. The in-store browser path is not "equivalent" after this change; it is unchanged.

Where a base comes from when it is not empty is settled the same way the token is: **the base and the
token are one record**, written when the device pairs and read together afterwards. The operator
scans or types the edge's address, the app pairs against it, and both halves are stored. They belong
together because a token is only meaningful against the edge that issued it — a token carried to a
different base is a `401` waiting to happen, and a base with no token is a device that has not
paired.

One asymmetry, and it is deliberate: the existing `401` handler calls `clearDeviceToken()`, and after
this change it clears **the token and not the base**. A device whose token went stale must re-pair to
*the same edge*; an app that forgot the address would send the operator to look for a QR code that
may be in another building, or on a screen nobody can reach.

`/ws` derives from the same base rather than from `window.location`:

- base empty — today's expression, `${scheme}://${window.location.host}/ws` with `scheme` from
  `window.location.protocol`, unchanged.
- base set — resolve `/ws` against it and swap the scheme: `https:` → `wss:`, `http:` → `ws:`.

The device token still travels as the second entry in the subprotocol list, for the reason `ws.rs`
gives and this record does not revisit: a query parameter would put a credential in the request path,
and the edge logs the request path.

### The token goes where the operating system will keep it

`localStorage` stays the store for a plain browser. It is what a browser has, the existing `try`/`catch`
wrappers already handle a browser that refuses it, and nothing about the in-store till changes.

A native shell uses the OS credential store, and the framework already knows exactly which one and
already owns the code. [`pos_ports::key_vault::KeyVault`](../../crates/pos-ports/src/key_vault.rs) is
the port — "where a machine keeps its own credentials", async because a keyring call is genuinely
I/O, with `SecretName` a closed `#[non_exhaustive]` enum so that a typo cannot create a second secret
nothing reads. [ADR-0086](0086-edge-keyvault-and-activation.md) built the adapter behind it:
[`key-vault-keyring`](../../crates/adapters/key-vault-keyring/src/lib.rs), over the `keyring` crate,
Windows Credential Manager and macOS Keychain and Linux keyutils, with the OS call isolated behind a
`KeyringBackend` seam so the adapter's own logic is proven in the fast gate.

So the design is: the client's token access becomes a two-method seam — read, write, clear — with a
`localStorage` implementation that is today's code moved and a native implementation that calls the
shell. Where the shell has a Rust side, that side implements the same `KeyVault` port over the same
adapter, with one additive `SecretName` variant for the device token. A shell holding a device
credential is the exact case the port's own module documentation describes; writing a second
credential store beside it would be inventing a parallel answer to a question that has one.

Two honest limits. First, `SecretName`'s existing variants are all secrets a *store server* holds,
and a device token is a secret a *device* holds — the variant is additive and the discipline
transfers, but this is the first entry that is not about the box. Second, and larger: **no shell has
been spiked.** Tauri v2 against the real `ui/dist` on Android is unproven, and this record does not
pretend otherwise. What survives the spike failing is the seam: whatever shell ships implements the
same two methods, and a plain browser keeps `localStorage`, which is what every till in the estate
uses today.

### The pairing URL gets a second form; the raw-IP one does not move

`pairing_url` stays exactly as it is, and so does `EdgeConfig::advertised_ip` and its
`advertised_host()` fallback to the bind IP. ADR-0030's field constraints have not changed: Chrome on
Android still does not resolve mDNS, a DHCP reservation still pins the address, and an operator in a
shop still needs a URL that depends on no name resolution at all. An `in-store` placement mints the
same string it mints today.

A placement whose devices are not on its LAN mints `https://<host>/pair?code=NNNNNN` instead, from a
new optional `EdgeConfig` field naming the edge's own public origin. When that field is set it wins,
because a box that has been given a public origin is a box whose devices are not on its LAN.

**`https` is required in that form, not preferred.** A bearer token crossing a WAN in clear text is a
bearer token given away, and ADR-0030's second field constraint — an in-browser camera needs a secure
context — bites harder here than in a shop, because the hosted pairing URL is the one an operator is
most likely to scan rather than type.

The public origin lives in `config.toml` with `bind` and `advertised_ip`, **not** in the config tree,
and that is not an inconsistency with the node three sub-headings above. ADR-0004 already drew this
line and named this exact exception: *"Anything a store genuinely must set alone (network address for
pairing) is an explicit, narrow exception."* The allow-list says **who may address the edge** — a
fleet decision about shells, which the cloud owns. The public origin says **where this box is** — a
fact about one machine's network, which the machine's operator or (in mode 3) the platform that
started it knows and the cloud does not.

### The version handshake is a response header, because drift shows up on a response

**The edge stamps its release version on every `/api/*` response as `pos-edge-version`.**

Not the pair response. A version read once at pairing time is a version that was true once. The drift
this exists to catch happens *after* pairing — an OTA ring moves the edge on a Tuesday, or the app
updates itself overnight — and by then the pair response is a memory.

Not `/healthz`, even though it already carries the right two numbers. It would work, and it is worse
in three ways: the app must poll it, which is a guess about when to look; the guess is wrong exactly
when it matters, because the moment of interest is a call that just failed; and it would put a route
whose purpose is a service manager's liveness probe onto the origin allow-list to serve a caller it
was not built for.

A response header has none of those problems. It rides the response the app already made, including
the `404` for the route one side moved — so the app fails and learns *why it failed* in the same
message.

The details that make it correct:

- **`Access-Control-Expose-Headers: pos-edge-version`.** Without it a cross-origin response's headers
  are invisible to the page and the whole mechanism silently does nothing. This is the same class of
  omission as a missing `Vary`, and it is the reason the header cannot be added without the CORS
  work landing beside it.
- **It carries the release version and nothing else.** `PROTOCOL_VERSION` is the edge↔cloud wire
  language ([ADR-0024](0024-protocol-version-negotiation.md)); the app is not on that wire, and
  [`docs/naming-and-api.md`](../naming-and-api.md) §4 is firm that the three existing version axes
  are never mixed. This introduces no fourth axis — it publishes, on a second rail, the value
  `version.rs` already stamps from the release tag and `/healthz` already reports.
- **The comparison is one-sided, and the additive rule is why.** `AGENTS.md` §2 forbids removing or
  renaming a published field, event or permission, so a *newer* edge never takes away a route an
  older app calls. Only "the app is ahead of its edge" can break, so only that direction is checked:
  the app carries the minimum edge release it was built against and compares.
- **`0.0.0` never warns.** [`version.rs`](../../crates/pos-edge/src/version.rs) makes `0.0.0` the
  honest answer for a hand-built binary, and it sorts below every real release. A developer running
  `just run-edge` against a shipped app should not see a banner on every call.
- **A behind-edge does not stop the till selling.** It shows a banner naming both versions and
  carries on. ADR-0024 already settled the principle for the tier below: *"a protocol mismatch
  degrades to 'not syncing', never to 'not selling'."* A version string is not a reason to refuse a
  customer.

There is a lesson in the same document this header must not repeat. `pos-api-version` was removed
because *"Every route ignored it, so an integrator who sent it believed they had pinned something and
had not — worse than no header at all."* `pos-edge-version` is added in the same change as the client
code that reads it and the banner that shows it, or it is not added.

### The S4 pairing budget is still right, and is no longer sufficient on its own

The budget in [`pairing.rs`](../../crates/pos-edge/src/pairing.rs) — ten consecutive failed
redemptions shut `POST /api/pair` for sixty seconds, one counter for the whole box, checked before
the code table is touched, cleared by a success, deliberately not persisted — is unchanged. Its
arithmetic does not depend on where the caller is:

```rust
/// Ten tries a minute walks a million codes in about sixty-nine days, against a code that lives five
/// minutes — so the budget is not a speed bump, it closes the attack.
```

Sixty-nine days against a five-minute code is the same number whether the attacker is on the shop
switch or in another country. **Guessing is still closed.**

What changes is not confidentiality, it is availability. In-store, spending a store's budget required
being on its LAN. For a hosted placement it requires a script, and ten wrong codes from anywhere shut
pairing for a minute — including for the operator standing at the counter holding a real code. That
is a denial-of-service lever the in-store deployment did not hand out.

**The budget is not re-keyed to fix it, and per-IP keying in particular is refused.** A hosted
placement sits behind a proxy, so the peer address is the proxy's unless a hop count is configured
correctly — `pos_cloud` already carries `trusted_proxy_hops` for exactly this and
`crates/pos-cloud/src/main.rs` warns that *"a wrong `trusted_proxy_hops` is a wrong rate-limit key"*.
And an attacker with an IPv6 allocation has a /64 to rotate through. A budget keyed on a value the
attacker chooses is not a budget; it is a counter that never reaches its limit while the operator's
single address does.

**`MAX_FAILED_REDEMPTIONS` and `REDEEM_LOCKOUT` stay compiled-in constants, not configuration.** A
tunable rate limit is a rate limit somebody widens at 19:30 on a Friday because "pairing is broken",
and never narrows again.

**The proxy carries the new load.** A hosted placement is fronted by a reverse proxy — it needs one
anyway, for the TLS the `https` pairing URL requires — and that proxy rate-limits `POST /api/pair` by
source address before the request reaches the edge. Two layers with a clean division: the proxy has
the addresses and the capacity to absorb a flood, and the box-wide budget is the last line that works
with no proxy in front of it at all, which is precisely the `in-store` case. Neither is asked to be
the whole answer, and the S4 budget was never designed to be one for an internet-facing endpoint.

## What this deliberately does not do

- **No wildcard origin, ever, in any mode.** `Access-Control-Allow-Origin: *` is not a fallback, not
  a development convenience and not a setting. A `dev-ui` build reads assets from disk
  ([ADR-0018](0018-http-websocket-stack.md)) and is still served by the edge on the edge's origin; a
  developer who needs a second origin publishes one to their own store like everybody else. A
  wildcard that exists behind a feature flag is a wildcard one build script away from production.
- **No cookie-based authentication for the app, and no credentialed CORS.** Declined permanently, not
  postponed. Every property that makes a cookie convenient — the browser attaches it automatically,
  across origins, without the page asking — is the property that makes it wrong here, and the moment
  `Allow-Credentials: true` appears beside an allow-list, a cross-site request forgery becomes a
  settled bill. The bearer token is not merely adequate; it is the reason there is no CSRF surface
  to defend.
- **No rendering, pricing or tax on the device.** The app is a front-end to routes. The edge still
  renders the receipt and the kitchen ticket, still rasterises non-ASCII scripts
  ([ADR-0102](0102-printing-any-script.md)) and still composes the legal invoice block
  ([ADR-0106](0106-the-store-is-a-legal-person.md)), and the check the till shows still comes from
  `GET /api/tables/{id}/check` rather than being added up locally. ADR-0110 declined a device-local
  write buffer outright; this record does not smuggle half of one in as "just the pricing".
- **It does not weaken the in-store path anywhere.** The default base is the empty string. A store
  with no `origins` node gets no CORS layer behaviour it did not have, because same-origin is allowed
  unconditionally. `pairing_url` is untouched, `advertised_ip` is untouched, the two auth gates are
  untouched, and `localStorage` remains the token store for every browser in every shop.
- **It does not put CORS on `pos_cloud`.** The console is served by `pos_cloud` and talks to it
  same-origin, exactly as the till does to the edge. There is no CORS in `pos-cloud` today and none
  is added here. If a second console front-end ever wants one, it is a different record with a
  different threat model — `/admin` carries a session cookie, which is precisely the combination this
  record refuses for the edge.
- **It does not give a hosted edge its TLS.** `https` in the pairing URL requires a certificate, and
  [ADR-0090](0090-tls-postures.md)'s four `TLS_MODE` values describe Caddy in front of `pos_cloud`,
  not `pos_edge`, which terminates nothing today. A `hosted-by-operator` placement is the operator's
  own proxy; `hosted-by-platform` is ADR-0113's, along with the hostname the public origin field
  would be set to.
- **It does not report pairing refusals to the fleet console.** The edge logs a shut-out with its
  failure count, and that is where it stays. Putting a refused-redemption counter in the heartbeat's
  optional JSON body ([ADR-0068](0068-fleet-liveness.md)) so the alert engine
  ([ADR-0073](0073-alerting.md)) can fire on a hosted store being probed is an obvious next step and
  is genuinely useful; it is not decided here, because nobody has yet said what threshold means
  anything, and an alert with a guessed threshold trains people to ignore alerts.
- **It does not build a shell, and it does not assume one works.** Tauri v2 against the real
  `ui/dist` on Android has not been spiked, and Android as an ESC/POS print agent has not been spiked
  either — both belong to ADR-0112. Nothing above depends on either succeeding: if no native shell
  ever ships, this record still delivers the base URL, the origin allow-list and the version header,
  which is what a hosted placement and a second front-end need on their own.
- **It does not know the literal origin strings a native shell will use.** They depend on the shell
  and the platform, and the spike has not run. The node therefore validates the *shape* of an origin
  — an exact serialised origin, no path, not `null`, no wildcard — rather than an allow-list of
  schemes it would have to guess. That is a real piece of leniency and it is recorded here rather
  than hidden in the validator.

## Consequences

- `pos-edge` gains a CORS layer by enabling `tower-http`'s `cors` feature — a feature flag on a
  dependency already in the graph, not a new crate. It is layered **outside** both auth middlewares
  in [`http/mod.rs`](../../crates/pos-edge/src/http/mod.rs), so a preflight is answered before
  `require_paired_device` refuses it.
- `AppState` ([`state.rs`](../../crates/pos-edge/src/state.rs)) gains an `Arc`-held origin list that
  the config-pull loop replaces wholesale, alongside `pairing` and `fanout`. It is read on the front
  of every request and is bounded at eight entries.
- The config tree gains an `origins` node on the Brand layer, `apply_document` gains a branch for it
  under the never-blank rule, and the cloud gains a publish route behind
  `ConsolePermission::ManageStores` ([ADR-0067](0067-multi-admin-console-rbac.md)) with `If-Match`
  ([ADR-0094](0094-console-optimistic-concurrency.md)) and an audit entry naming the acting admin
  ([ADR-0069](0069-audit-trail.md)) — the same three things every other `/admin` write carries.
- **`origins` makes ADR-0033's deferred fan-out cost visible.** Its value is identical for every store
  under a brand, and ADR-0033 records that *"a shared Tenant/Brand layer that fans out to every store
  under it is a future modeling step; today each store's tree holds its own four layers."* So
  shipping a new app shell to 500 stores is 500 publishes. This record does not fix that; it is the
  first node for which the missing fan-out is the dominant cost rather than a tidiness complaint.
- Every covered route now answers `OPTIONS`. That is twenty-five new method/route pairs in the
  router's surface, and the test that asserts the route list has to grow with them.
- [`docs/naming-and-api.md`](../naming-and-api.md) §4's header table gains a `pos-edge-version` row in
  the same change as the client code that reads it. The table is described there as the contract and
  the code is checked against it; a header with no reader has been removed from that table once
  already.
- [`ui/src/api/client.ts`](../../ui/src/api/client.ts)'s opening comment stops being true and is
  rewritten in the same pull request. The file gains a base-URL accessor defaulting to `""` and a
  token-store seam; [`live.ts`](../../ui/src/api/live.ts) derives its socket URL from the base
  instead of `window.location`.
- `EdgeConfig` gains an optional public-origin field beside `advertised_ip`, and `pairing.rs` gains a
  second URL constructor. `pairing_url` itself, its `IpAddr` parameter and its test
  (`the_pairing_url_carries_the_code_over_raw_ip`) are unchanged.
- `SecretName` gains one variant for a device token — additive on a `#[non_exhaustive]` enum, and the
  first entry in it that names a secret held by a device rather than by a store server. `SecretName::ALL`
  is what the wipe-on-revocation routine iterates, so the new variant is covered by that sweep from
  the day it lands.
- **The test matrix gains a third axis.** ADR-0110 already doubled it by placement. Every covered
  route now also has a same-origin case, an allow-listed cross-origin case with its preflight, and a
  refused-origin case — and the refused case must assert that the *response* is refused by the
  browser rule, not that the handler ran differently, because the handler does not know.
- Nothing is removed and nothing is renamed. `PROTOCOL_VERSION` does not move: the app is not a party
  to the edge↔cloud protocol, and a response header, a config node, an optional config field and a
  `SecretName` variant are all additions. An edge published no `origins` node behaves exactly as this
  fleet's 500 stores behave today.
