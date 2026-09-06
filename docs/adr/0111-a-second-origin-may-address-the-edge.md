# ADR-0111 — A second origin may address the edge, from a list the cloud publishes

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-06
· Serves [ADR-0110](0110-edge-placement-is-a-deployment-axis.md)'s hosted edge placements
· Extends [ADR-0030](0030-pairing-and-offline-auth.md)'s pairing and
[ADR-0084](0084-device-authentication.md)'s bearer gate without moving either
· Sources its list from [ADR-0004](0004-cloud-owned-configuration.md) over
[ADR-0033](0033-config-tree.md)'s layers
· Relates to [ADR-0018](0018-http-websocket-stack.md),
[ADR-0024](0024-protocol-version-negotiation.md),
[ADR-0086](0086-edge-keyvault-and-activation.md),
[ADR-0091](0091-durable-edge-auth-state.md)

This is the second of the five records on the **Edge Anywhere** programme, after
[ADR-0110](0110-edge-placement-is-a-deployment-axis.md) and beside
[ADR-0112](0112-print-agents.md) (the print agent), [ADR-0113](0113-the-host-agent.md) (the host
tier and the console's Start button) and
[ADR-0114](0114-region-is-required-recorded-visible.md) (region as a required, recorded attribute).
All five are on disk, so all five are linked.

The store attribute those records select on is `edge_placement`, never a bare "placement" — the word
is already taken by `MenuPlacement`, an item's placement in a menu, and by the OTA rollout placement
at `/admin/config/ota/placement`. [ADR-0110](0110-edge-placement-is-a-deployment-axis.md) states the
rule in full; this record follows it. The Rust type is `EdgePlacement` and the wire values are
`EDGE_PLACEMENT_UNSPECIFIED`, `EDGE_PLACEMENT_IN_STORE`, `EDGE_PLACEMENT_HOSTED_BY_OPERATOR` and
`EDGE_PLACEMENT_HOSTED_BY_PLATFORM`.

## Delivery — 2026-09-06, the allow-list half

The **origin allow-list** shipped: the node, the one rule that validates it, the cloud route that
authors it, the console card that publishes it, the CORS layer on every covered route, and `/ws`'s own
`Origin` check.

- **The node.** [`pos_proto::origins`](../../crates/pos-proto/src/origins.rs) holds `PublishedOrigins`,
  `MAX_ORIGINS = 8`, and `validate_origins` — the four refusals this record argues for. It lives in
  `pos-proto` rather than on the edge because the cloud refuses a bad origin at authoring time and the
  edge refuses one at apply time, and two copies of that rule would drift into an edge quietly dropping
  what the console said it saved.
- **The writer.** `GET`/`PUT /admin/config/origins`
  ([`http.rs`](../../crates/pos-cloud/src/http.rs), on `config_channels_router`), behind
  `ConsolePermission::PublishConfig` and audited as `config.origins.publish`. A refused entry is a
  `400` naming the entry and the rule it broke.
- **The console.** A fifth card on **Channels & payments**, beside `channels`, `tender`, `qr` and
  `vendors` — the same publish-a-node-to-one-store shape, and the same store gate.
- **The carrier.** [`pos_edge::origins`](../../crates/pos-edge/src/origins.rs)'s `Origins`, held as one
  `Arc` on `AppState`, written by the config-pull loop's `apply_origins` and read by the request path
  — shaped like `Pairing`, for the reason "The list needs a carrier the router can read" gives.
- **Coverage.** Applied by each router constructor to its own named subset, never to a merged
  application. Building it that way caught a live defect while this slice was being written: axum's
  `Router::layer` covers every route added *before* it, so the first wiring silently covered
  `/healthz` and `/ws` as well. `/api/pair` is now its own sub-router, and
  [`tests/origins.rs`](../../crates/pos-edge/tests/origins.rs) pins that `/healthz` carries no CORS
  header — so a later refactor that layers at the top fails rather than quietly widening the surface.
- **`/ws`.** `require_permitted_origin_ws`, outermost over the device-token gate, refusing a
  cross-origin upgrade `403`. A handshake with no `Origin` is allowed and reaches the token gate, as
  this record's three-case table says.

**Not in this slice, and each still open.** The base-URL default, the token in the operating system's
keychain, the second pairing-URL form, the `PROTOCOL_VERSION` response header, the
`docs/snapshots/routes.txt` additive-route gate, and the print agent's four routes (which
[ADR-0112](0112-print-agents.md) builds). The allow-list is what those need to exist first; none of
them is blocked on a decision.

## Delivery — 2026-09-06, the base URL and the credential seam

`request()` takes a base, and its default is the empty string — so an in-store till sends the
identical root-relative bytes it sent yesterday. Not `window.location.origin`, which would produce an
absolute URL where a root-relative one goes today: a different request string, a different set of
things that can go wrong with it, and a change to the one path that is currently working in every
shop. The in-store path is not "equivalent" after this change; it is unchanged.

**All three `fetch` call sites take it.** `signIn` and `signOut` bypass `request()` on purpose — one
reads a structured refusal rather than throwing, the other wants no body — and changing only
`request()` would have shipped a shell that can read the floor and settle a bill but can never sign
an employee in. The three session routes are on the covered list precisely so a second origin can
sign in.

`/ws` derives from the same base: empty gives today's expression from `window.location`, and a set
base is resolved against with the scheme swapped, because a socket opened on `https:` rather than
`wss:` is refused rather than downgraded. The device token still travels as the second subprotocol,
for the two reasons the tree already gives.

**The base and the token are one record**, written together by `pair(code, base)` and read together
afterwards — with the asymmetry this record calls for: a `401` clears the token and **not** the base.
A device whose token went stale must re-pair to *the same edge*; an app that forgot the address would
send the operator looking for a QR code that may be in another building.

`ui/src/api/credentials.ts` is the seam: `read`, `write`, `clear`, with the browser implementation
installed by default and `installCredentials` for a shell that reaches the OS credential store
through [ADR-0086](0086-edge-keyvault-and-activation.md)'s existing adapter. Two storage keys rather
than one blob, so a till that paired before this existed keeps its token exactly where it left it —
the read is the same read it always was, and no device is asked to re-pair for a refactor. **No shell
has been spiked**; what survives that is the seam.

**Verified red then green in a real browser**, against a real edge, by the replay harness: with the
base defaulted to `""` all thirteen declared flows pass — pairing, sign-in, seating, firing,
settling, bumping, the shift — and with it pointed at a dead origin twelve of the thirteen fail. That
is the property this slice most needed proved, because it is a claim about *unchanged* behaviour and
a green suite that never exercised the change would say nothing.

Which is nearly what happened, and is worth recording. `rust-embed` is configured with
`debug-embed`, so `ui/dist` is compiled into the binary in **every** profile — a `pnpm build` alone
changes nothing the replay can see, and the first run of this experiment passed in both directions
against a bundle from before the change. `examples/minimal-edge` has to be rebuilt between the two.
[`ui/README.md`](../../ui/README.md) now says so where someone about to run the harness will read it.

**Still open from this record.** The second pairing-URL form, and the pairing screen field that lets
a shell operator supply the address this slice now stores.

## Delivery — 2026-09-06, the version handshake

`pos-edge-version` ships, and — as this record requires — it ships with the client that reads it and
the banner that shows it. The lesson it cites is `pos-api-version`, removed because *"every route
ignored it, so an integrator who sent it believed they had pinned something and had not"*. A header
nothing reads is worse than no header.

**The edge stamps every `/api/*` response**, including the asset fallback's. That last part is the
whole point and is what decided where the layer goes. The CORS layer is applied per sub-router
because coverage there is a *policy* — a route is covered because a constructor named it. This is a
fact about the binary, true of every `/api` answer it gives, and the answer that most needs it comes
from no `/api` sub-router at all: a path one side moved does not `404` on this edge, because
`assets::serve` returns `200 text/html` for anything unmatched. So the layer sits on the merged
application and tests the path itself. `/healthz` is not stamped — it serves a service manager, and
it already reports the version in its body.

`Access-Control-Expose-Headers` carries it, because without that a browser hides the header from the
page and the mechanism does nothing for the only caller that can drift from its edge at all.

**The app's side of the comparison is a build stamp, not a hand-maintained constant.** The release
workflow injects `VITE_MINIMUM_EDGE_VERSION` from the same tag, with the same `v` stripped, that
stamps the binary — so the bundle embedded in an edge always matches the binary serving it and never
warns about itself, while a shell built from that bundle and later pointed at an older edge is
exactly the case the banner is for. A local build sets nothing and reads `0.0.0`, which never warns,
the same honesty `version.rs` applies to a hand-built binary. A hand-maintained minimum was the
alternative and it rots in the direction that matters: nobody remembers to raise it the release they
start depending on a newer edge.

All three `fetch` call sites observe the header, not just `request()`. `signIn` and `signOut` bypass
that helper on purpose — one reads a structured refusal, the other wants no body — and they are three
of the routes a second origin most needs. The observation happens **before** the `ok` check, because
the call that just failed is the one an operator is looking at when they ask what version this box is
running.

Four properties verified red first, on the edge side: an `/api` answer carries the release; the asset
fallback's answer carries it too; `/healthz` does not; and a cross-origin response says the page may
read it. Removing the path test fails the third; removing `expose_headers` fails the fourth.

**Not verified by an automated test: the client comparison itself.** `ui/` has no unit-test runner —
its gates are `tsc`, the i18n lint and parity, contrast, and the step budget — so `edgeIsBehind`'s
three rules (`0.0.0` never warns on either side, an unparseable version never warns, strictly-older
warns) are held only by reading. The failure that escapes is a banner shown or hidden wrongly; it
cannot refuse a sale, which is why this was not the slice to add a test framework in. Adding one to
`ui/` is a decision on its own.

**Still open from this record.** The base-URL default, the token in the operating system's keychain,
and the second pairing-URL form.

## Delivery — 2026-09-06, the additive-route rule and its snapshot

The rule this record *extends* is now enforced. `AGENTS.md` §2 forbade removing or renaming a
published field, event or permission and said nothing about routes; it now names the edge's `/api/*`
routes alongside them, and `docs/snapshots/routes.txt` is what holds it, checked by
`cargo xtask snapshot` the same way the other three are.

This lands before the `pos-edge-version` header, not beside it, because the header's comparison is
**one-sided** — an app checks only whether it is ahead of its edge — and the section below is explicit
that the guarantee making that safe did not exist. Building the header first would have shipped a
comparison resting on a promise nothing kept.

Two mechanisms, because two different things rot:

- **`crates/pos-edge/tests/routes_snapshot.rs`** regenerates the file from the source and fails when
  the two disagree. Axum's `Router` exposes no route list, so the set is recovered from the source the
  way `pos_cloud::openapi_admin` recovers `/admin`'s — but by walking the whole `src/` tree rather than
  naming one file, because the edge registers routes from five modules and a sixth would otherwise be
  missed silently. It reads the method too, and every method on a chained registration rather than the
  first: a snapshot holding only the `GET` of a `get(read).post(write)` would let the `POST` go
  unnoticed. `POS_UPDATE_SNAPSHOTS=1` regenerates, which is how a route is added.
- **`cargo xtask snapshot`** answers the question the test cannot, because only git can: has a line
  *disappeared* since the base branch? Its hint is now chosen per file — the event-catalogue sentence
  sent a contributor who renamed a route looking for a payload field that does not exist.

**The scope is `/api/*`, which is this record's line and not a shortcut.** `/healthz` and `/ws` are
registered by the same router and are outside it, for different reasons that are worth stating rather
than leaving as an omission. `/healthz` serves a service manager's liveness probe, not the app. `/ws`
is one route named in one expression in `live.ts`, and renaming it fails at connect time — at once, on
every device — rather than as the unattributable parse error an `/api` rename produces, because the
asset fallback answers an unmatched path with `200 text/html`. A test pins that boundary, so widening
it later is a visible act rather than a side effect of touching the extractor.

Verified red first in both directions: renaming `/api/floor` fails the snapshot test, and deleting a
line from the committed file makes the `xtask` gate refuse with the route hint.

**Still open from this record.** The base-URL default, the token in the operating system's keychain,
the second pairing-URL form, and the `pos-edge-version` response header — which is now unblocked.

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

**A hosted edge placement.** [ADR-0110](0110-edge-placement-is-a-deployment-axis.md) puts `pos_edge`
on a VPS or a platform region. A browser on the counter reaches it over a WAN by hostname, which is
fine — that path is still same-origin, because the app is still served by the edge. But one of
ADR-0110's own "what this deliberately does not do" bullets, *"It does not give the edge a second
address"*, is the honest statement of what is missing: `EdgeConfig::advertised_ip` has nothing to
mean, and the client's "so it works on the store LAN with no configuration" keeps its first clause
and loses its second. A device that is not on the edge's LAN has no supported way to be told where
the edge is.

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
resolution at all. Every word of that is still true on a shop LAN. None of it describes an edge
placement reachable only over the internet, where there is no LAN IP to print, and where a bearer
token crossing a WAN in clear text is not a thing anyone should ship.

### Two release trains, and nothing tells anyone they have diverged

The edge ships through OTA rings. An app in an app store ships when a review board says so, onto
devices that update when their owners let them. Those two clocks do not agree, and at 500+ stores
they will not even agree within one brand.

Today nothing would report the disagreement, and the shape of the failure is worse than a bad error
message. `/healthz` reports `version` and `protocol_version`, but the app has no reason to call it
and no rule about what to do with the answer. And a call to a route the edge does not have does not
`404`: [`http/mod.rs`](../../crates/pos-edge/src/http/mod.rs) ends with `.fallback(assets::serve)`,
and [`assets.rs`](../../crates/pos-edge/src/http/assets.rs) is explicit about what that means —
*"An unknown path is not a 404: it is a client-routed path the single-page app will resolve, so it
receives `index.html`."* So the app asks for a route its edge has never heard of and gets **`200
text/html`**. `request()` in [`client.ts`](../../ui/src/api/client.ts) takes the `response.ok`
branch, calls `await response.json()` on a page of HTML, and throws a `SyntaxError` — not an
`ApiError`, with no status and no `statusText` for anyone to read.

That is a support call that starts with "it just stopped working", produces a JSON parse error in a
console nobody is looking at, and ends hours later with somebody comparing version strings. The
client cannot currently tell a missing route from a valid response, and no amount of care in the
screens fixes that, because the information is not in the response.

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
and applied by the config-pull loop in
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
  the serving origin — defined below, because the edge cannot simply read one off itself — is allowed
  with no list at all. Without this rule, a store with no `origins` node published would refuse its
  own UI, and the never-blank contract would turn a malformed publish into a dark shop.
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

### The list needs a carrier the router can read, and `session_from_config` is not one

This is the seam the node needs and the tree does not have, so it is decided here rather than left
to implementation.

[`session_from_config`](../../crates/pos-edge/src/config_client.rs) is what actually applies a
pulled document today — `menu`, `permissions`, `capabilities`, `floor`, `stations`, `tax`,
`campaigns`, `inventory`, `channels`, `tender`, `qr`, `locale`, `fleet_update`, `device_ota`,
`lease` — and it is the wrong shape for this one. It is a pure function from a base `EdgeSession` and
a document to a new `EdgeSession`, and an `EdgeSession` is what the *application* layer decides
against. The CORS layer is not in the application layer; it runs on the front of every HTTP request,
before any handler, and what it reads has to be reachable from
[`AppState`](../../crates/pos-edge/src/state.rs) — which holds `config`, `build`, `fanout`, `clock`
and `pairing`, and is built once at start-up.

**So `origins` gets a carrier shaped exactly like `pairing`: an `Origins` value with interior
mutability, held as `Arc<Origins>` on `AppState`, constructed in `compose` and cloned into the CORS
layer.** `Pairing` already solved this problem — it is state that a request path reads and a
background path writes, so it holds its `Mutex` inside and everything shares one `Arc`. `Origins`
holds one `Mutex<Vec<Origin>>`, bounded at eight, with a `replace` the config-pull loop calls and a
`contains` the layer calls. No new dependency: `std::sync::Mutex` is what `Pairing` uses.

The config-pull loop gets a handle to the same `Arc<Origins>`, the same way it already holds a handle
to the `Edge` it hot-swaps sessions into. The `origins` branch is therefore *beside*
`session_from_config` rather than inside it — the loop parses and validates the node and calls
`Origins::replace`, and the never-blank rule is enforced there: a document with no `origins`, or a
list that does not validate, is not replaced.

Doing it any other way means the router reading application state on every request, which is the one
thing `AppState`'s cheap-to-clone shape exists to avoid.

### One rule decides membership, and today it covers twenty-six of the edge's twenty-seven routes

**A route is covered if a paired device calls it in the course of trading or of becoming a paired
device. A route that provisions the machine itself is not, and neither is anything that is not part
of the app.** That is the rule, stated before the list, so a route added next month inherits an
answer instead of needing an amendment to this record.

The edge serves twenty-seven `/api/*` routes today (two of them, `POST /api/activate` and
`GET /api/activation`, only on a box provisioned for a cloud) plus `/ws` and `/healthz`.

**Covered — twenty-six.** Everything a till does: the eighteen guarded domain routes, the three
session routes, `/api/orders/open`, `/api/pair`, `/api/pair/devices`, `/api/pair/revoke`, and
`GET /api/activation`. These are the surface the app is for, and they do not all carry the same
gates ([`http/auth.rs`](../../crates/pos-edge/src/http/auth.rs)):

- **Nineteen sit behind both gates** — `require_paired_device` then `require_signed_in`. Those are
  the eighteen in `domain_router`'s `guarded` sub-router plus `/api/orders/open`, which
  `counter::router` is layered with separately. A cross-origin caller reaching one of them still
  needs a device token *and* an employee signed in on that device, so CORS adds a third condition to
  routes that already had two.
- **Five sit behind the paired-device gate only.** The three session routes, because signing in is
  how a device passes the second gate and `http/mod.rs` says so in as many words — *"a paired device
  signs a person in and out here, so these sit behind the paired gate but not the signed-in one"* —
  and `/api/pair/devices` and `/api/pair/revoke`, which retire a device and are as strong as pairing
  and no stronger.
- **Two sit behind neither.** `POST /api/pair` is unauthenticated by design, argued below.
  `GET /api/activation` returns one boolean.

That distribution is the honest picture, and it matters: `POST /api/session/sign-in` takes a badge
code and a PIN and is one of the five, so a cross-origin caller reaching it needs a device token and
nothing else. Seven of the twenty-six do not require a signed-in employee, and the wildcard argument
below is about exactly those seven.

**Covered — `GET /api/activation`, and this is a change from the obvious answer.** It reads as an
activation route and belongs with `POST /api/activate`, but the shipped app disagrees:
[`App.tsx`](../../ui/src/App.tsx)'s `onMount` calls it on **every boot**, ahead of pairing and ahead
of sign-in, and routes the operator to `/setup` when the box is not activated. It is the first call
any front-end makes. Leaving it same-origin-only means a second origin's very first request fails —
softly, because the call is wrapped in `.catch(() => routeDevice())`, which is worse than loudly: an
unactivated hosted box would silently never route anyone to `/setup`. And the route returns a
standing boolean, not a secret. So it is covered.

**Not covered — `POST /api/activate`.** This is where the argument for excluding activation actually
lives. It exchanges a code from the store's setup sheet for a long-lived machine credential that
lands in that box's OS keyring ([ADR-0086](0086-edge-keyvault-and-activation.md)); a route that mints
a machine credential should not be reachable from a page on another origin, and there is no
cross-origin actor in that story. An operator activates at the `/setup` screen the box itself serves,
and a hosted edge placement that must be activated is activated from the console-driven flow
[ADR-0113](0113-the-host-agent.md) builds, not from a till.

**Not covered — `/healthz`.** It is what a service manager polls. A service manager is not a browser
and never sends `Origin`, so CORS headers on it would serve nobody, and putting it on the list would
publish `store_id` to a page for no gain.

**Not covered — the asset fallback** (`http::assets::serve`). Cross-origin `fetch` of `index.html` is
not a use case; a shell that wants the app bundles it.

**`/ws` is covered by a different mechanism**, not by CORS, for the reason its own sub-heading gives.

### The print agent's four routes are covered, by the rule and not by an amendment

[ADR-0112](0112-print-agents.md) adds four `/api/*` routes in the same programme —
`POST /api/print/agent`, `POST /api/print/agent/revoke`, `GET /api/print/jobs` and
`POST /api/print/jobs/{job_id}/ack`. A print agent running in a native shell is precisely the
cross-origin caller this record exists for: it is a paired device, it is not served by the edge, and
if its long-poll is refused by the browser rule then nothing prints.

**All four are covered, and they do not all carry the same gates.** ADR-0112 sets them and the census
above has to match it, so it is spelled out rather than summarised. `POST /api/print/agent` and
`POST /api/print/agent/revoke` sit behind the paired-device gate **and** a signed-in employee holding
`Permission::ManageDevices` — claiming or releasing a printer is a human act, so those two are
*stronger* than `/api/pair/revoke`, not equal to it. `GET /api/print/jobs` and
`POST /api/print/jobs/{job_id}/ack` sit behind the paired-device gate alone, because an agent is an
unattended process and a PIN before every kitchen ticket would be absurd; those two are the ones
comparable to `/api/pair/devices`.

The distinction is not pedantry here: this record's census turns on which routes lack the signed-in
gate, and the wildcard argument below is about exactly those. The two job routes join that group; the
two human acts do not. This paragraph exists only to record that the two records were checked against
each other rather than left to assume. When ADR-0112 lands, the edge serves thirty-one `/api/*`
routes, thirty are covered, and nine rather than seven lack a signed-in employee.

A `pos_print_agent` that is a headless native process rather than a browser is not bound by CORS at
all; it sends whatever `Origin` it likes, or none. The coverage matters for the shell case, where the
agent's polling runs inside a webview on the shell's own origin.

### Pairing is on the list, and the reason it is safe is not the allow-list

This is the decision worth arguing, because the instinct is to keep the unauthenticated door
same-origin-only.

That instinct fails on the native app. If `POST /api/pair` is same-origin-only, the app cannot pair
at all; the best it could do is open a webview on the edge's own origin, redeem a code there, and
inherit the token from *that* origin's storage. Which puts the credential back in web storage on the
edge's origin, defeating the keychain decision three sub-headings down, and adds a second pairing
path that only one kind of client uses. ADR-0110 is explicit that the moment a path forks on
edge placement the framework has two systems and tests one; a second pairing path is that fork one
lower.

The alternative shape is cloud-brokered pairing, and [ADR-0030](0030-pairing-and-offline-auth.md)
rejected it outright as option 2 — *"Pairing a new tablet during an internet outage is exactly when a
store needs it to work."* That refusal is about `in-store`, where it still binds absolutely. It would
be possible to argue that a hosted edge placement has no offline pairing to protect, so it may broker
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
edge placement, the proxy in front of it — settled at the end of this section.

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

`*` is refused for a narrower reason, and the arithmetic is the argument. Nineteen of the twenty-six
covered routes sit behind both auth gates and would survive a wildcard: a page that has neither a
device token nor a signed-in employee gets a `401` or a `403` whatever its origin. **Seven would
not**, and three of them are worth naming individually:

- **`POST /api/pair`** is unauthenticated by design, because it is how a device obtains the
  credential. `Allow-Origin: *` hands every page on the internet a redemption attempt against every
  reachable edge, which means any page can spend a store's S4 budget from every browser that loads
  it and keep pairing shut for as long as it has visitors.
- **The three session routes** carry the paired gate and not the signed-in one, so under a wildcard
  any page a paired device visits can drive `POST /api/session/sign-in` — badge code and PIN — with
  that device's token. [`Lockout`](../../crates/pos-edge/src/auth.rs) rate-limits it, but its
  `HashMap<EmployeeId, Record>` is keyed per employee, not per origin, so the same wildcard that
  invites the attempts also lets a page lock a named member of staff out of their own till.
- **`POST /api/pair/revoke`** with an absent `device_id` is the revoke-all break-glass —
  `revoke_all()`, logged as *"revoking every paired device"*. It carries the paired gate only. A
  wildcard makes "every page a paired till visits can un-pair the whole store" a true sentence.

The remaining two are `GET /api/pair/devices`, which tells any page how many devices a store has
paired, and `GET /api/activation`, which tells it whether the box is set up — small on their own,
and exactly the reconnaissance a wildcard hands out for free.

An allow-list of one to eight strings costs nothing and removes all seven entirely.

### The preflight is the real cost, and `Vary: Origin` is the real bug

`Authorization` is not a CORS-safelisted request header, so **every cross-origin `/api/*` call is
preceded by an `OPTIONS` preflight**. Four things have to be right, and each of them breaks
something specific if it is not:

**The CORS layer sits outside both auth middlewares.** A preflight carries no `Authorization` header,
by specification. If the layer is applied inside `require_paired_device`, every preflight is answered
`401` and every cross-origin call fails — and the failure reads to an operator as "pairing is
broken", which is the worst possible mislabelling of a routing mistake. The layer short-circuits
`OPTIONS` before the gates run.

**There is exactly one layer value, built in `compose` and handed to the three router
constructors.**
This has to be said precisely, because the obvious place to put it does not exist.
[`http/mod.rs`](../../crates/pos-edge/src/http/mod.rs) builds two routers and merges neither:
[`server.rs`](../../crates/pos-edge/src/server.rs) does the merge —
`crate::http::router(state).merge(crate::http::domain_router(edge, …))` — and `compose_cloud_surface`
merges the activation router onto the result afterwards. Layering CORS on that merged application is
the only single point that reaches all twenty-six covered routes, and it is wrong: it would also
cover `/healthz`, `/ws`, the asset fallback and `POST /api/activate`, every one of which this record
declares not covered.

So the layer is a value, not a location. `compose` builds one `CorsLayer` from the shared
`Arc<Origins>` and passes it to each constructor, which applies it to its own covered subset:

- `domain_router` applies it last, after `guarded.merge(session).merge(counter)` and after
  `require_paired_device`, so it is outermost over all twenty-two domain routes.
- `router` applies it to `/api/pair` and to the `/api/pair/devices` + `/api/pair/revoke`
  sub-router, and to neither `/healthz`, nor the `live` sub-router that carries `/ws`, nor the
  `.fallback(assets::serve)`.
- `activation_router` applies it to `GET /api/activation` and not to `POST /api/activate`.

One value shared by three call sites means one policy, one origin list and one place to change the
allowed methods — and a route is covered because a constructor named it, never because it happened
to be under a layer somebody put at the top.

**The preflight response allows `authorization` and `content-type`, methods `GET`, `POST` and
`OPTIONS`, and is cached for ten minutes: `Access-Control-Max-Age: 600`.** Those are the only header
and method shapes the client sends. Caching matters more than it looks: without a max-age a hosted
edge placement pays two WAN round trips per call instead of one, and the `/ws` fan-out's under-50 ms
budget ([ADR-0018](0018-http-websocket-stack.md)) was measured across a shop, not across a WAN, so
the latency headroom is already gone.

**600 is chosen against the cap that binds, not against the cap that flatters.** Browsers clamp this
value: Chromium honours at most 600 seconds and Firefox at most 86400, so any larger number is a
number that is only ever true on one engine. Ten minutes is the whole of what the estate's browsers
will actually give, and a number the record states rather than leaving to whoever writes the layer.

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

- **`Origin` present, and neither the serving origin nor on the list** — refused before the upgrade.
- **`Origin` present and equal to the serving origin** — allowed, with or without a published list.
- **`Origin` absent** — allowed. A native client or a non-browser consumer sends none, and
  [`ws.rs`](../../crates/pos-edge/src/http/ws.rs) already documents that case: the endpoint accepts
  a client that offers no subprotocol and authenticates with the `Authorization` header instead.

### The serving origin is the request's own `Host`, and the scheme is not compared

This is the one place the record makes the edge refuse a request server-side, so getting it wrong
takes a store's own live link down. It is decided here rather than assumed.

**The edge cannot read its own origin off itself.** `EdgeConfig` carries `bind` and `advertised_ip`
([`config.rs`](../../crates/pos-edge/src/config.rs)) — a socket address and a LAN IP. Neither is a
scheme, and neither is the name a browser used. Worse, a hosted edge placement sits behind the
TLS-terminating reverse proxy this record requires for the `https` pairing URL, so the browser sends
`Origin: https://store.example.com` while the process sees a plain `http` listener on a private
address. Any rule that compares a scheme the edge has to guess refuses the store's own tills.

**So the serving origin is derived per request from that request's `Host` header, and the comparison
is on host and port only.** `Origin: https://store.example.com` against `Host: store.example.com`
matches; `Origin: http://192.168.1.10:8080` against `Host: 192.168.1.10:8080` matches. Both are the
app the edge served, in the two edge placements that exist, with nothing configured and nothing
published. A store with no `origins` node can never be refused its own socket, which is the whole
point of rule 1 above.

**Dropping the scheme from the comparison is a real concession and it is the right one.** It means
`http://store.example.com` would be treated as same-origin for a page served over `https` on that
name. That combination needs a browser to have loaded the app over plain HTTP on the proxy's own
hostname — which a proxy that terminates TLS either redirects or does not serve — and a page that did
load that way would be blocked from calling the `https` edge as mixed content anyway. The alternative
is an edge that guesses its scheme from a listener it does not terminate TLS on, and a wrong guess
there is a dark shop. A comparison that is slightly wide beats one that is confidently wrong.

The optional public-origin `EdgeConfig` field, added three sub-headings below for the pairing URL, is
**also** accepted as a serving origin when it is set. That covers the case where a proxy rewrites
`Host` to an internal name: the operator has already told the box what it is publicly called, and
there is no reason to make them say it a second time in the `origins` node.

The same derivation is what the `/api/*` CORS layer uses for its same-origin case, so there is one
definition of "same origin" in the edge and not two.

Be honest about what this buys. `require_paired_device_ws` is still the authority, and a page on an
unlisted origin cannot read the token out of another origin's storage, so it cannot offer the
subprotocol and cannot connect anyway. The `Origin` check is worth one string comparison for one
reason: `/ws` is the single endpoint a browser will open cross-origin unasked, so on the day a token
does leak — a screenshot, a support paste, a shared kiosk — it is the endpoint with no other barrier
in front of it. It is defence in depth, and it is described as that rather than dressed up.

### The base URL defaults to the empty string, so the browser path is byte-identical

`request()` gains a base: `fetch(base() + path)`, where the default value of `base()` is `""`.

**All three `fetch` call sites take it, not just `request()`.**
[`client.ts`](../../ui/src/api/client.ts) has three, and two of them bypass `request()` on purpose:
`signIn` calls `fetch("/api/session/sign-in", …)` directly so it can read a structured refusal body —
a wrong code, or a lockout with the instant it lifts — instead of throwing, and `signOut` calls
`fetch("/api/session/sign-out", …)` directly. Changing only `request()` would ship a shell that can
read the floor and settle a bill but can never sign an employee in, which is a worse failure than not
shipping one: the three session routes are in the covered list precisely because a second origin has
to be able to sign in. Either both take `base()` at their own call site or both move onto
`request()`; the record requires the first and permits the second.

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

The device token still travels as the second entry in the subprotocol list, for the two reasons the
tree already gives and this record does not revisit.
[`ws.rs`](../../crates/pos-edge/src/http/ws.rs) gives the first: the browser `WebSocket` API cannot
set an `Authorization` header, so the token has to reach the gate somehow, and the server selects
only the protocol *name*, so the credential does not travel back in the handshake response.
[`live.ts`](../../ui/src/api/live.ts)'s `#connect` gives the second, against the obvious alternative:
*"A query parameter was the alternative and is worse: the edge logs the request path, so the token
would end up in a log."*

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

Two honest limits. First, `SecretName`'s five existing variants are secrets held by a *store server*
or by a *cloud deployment* — `WebhookSigningKey` is *"the key that signs webhook deliveries from this
deployment"* and `VendorApiKey` is *"the per-tenant API key a cloud deployment uses against a
vendor"*, neither of which is a box in a shop. A device token would be the first held by a selling
*device*. The variant is additive and the discipline transfers, but the enum's inventory would then
span three kinds of machine rather than two, and nothing in the port says which kind a variant
belongs to. Second, and larger: **no shell has been spiked.** Tauri v2 against the real `ui/dist` on Android is unproven, and this record does not
pretend otherwise. What survives the spike failing is the seam: whatever shell ships implements the
same two methods, and a plain browser keeps `localStorage`, which is what every till in the estate
uses today.

### The pairing URL gets a second form; the raw-IP one does not move

`pairing_url` stays exactly as it is, and so does `EdgeConfig::advertised_ip` and its
`advertised_host()` fallback to the bind IP. ADR-0030's field constraints have not changed: Chrome on
Android still does not resolve mDNS, a DHCP reservation still pins the address, and an operator in a
shop still needs a URL that depends on no name resolution at all. An `EDGE_PLACEMENT_IN_STORE` store
mints the same string it mints today.

A store whose devices are not on its edge's LAN mints `https://<host>/pair?code=NNNNNN` instead, from a
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
fact about one machine's network, which the machine's operator or (under
`EDGE_PLACEMENT_HOSTED_BY_PLATFORM`) the platform that
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

A response header has none of those problems. It rides the response the app already made — including
the `200 text/html` the asset fallback returns for a route one side moved, which is the failure that
otherwise arrives as an unattributable `SyntaxError`. The app fails and learns *why it failed* in the
same message, and the header is on the response whether or not the body parses.

The details that make it correct:

- **`Access-Control-Expose-Headers: pos-edge-version`.** Without it a cross-origin response's headers
  are invisible to the page and the whole mechanism silently does nothing. This is the same class of
  omission as a missing `Vary`, and it is the reason the header cannot be added without the CORS
  work landing beside it.
- **It carries the release version and nothing else.** `PROTOCOL_VERSION` is the edge↔cloud wire
  language ([ADR-0024](0024-protocol-version-negotiation.md)); the app is not on that wire.
  [ADR-0024](0024-protocol-version-negotiation.md) is the document that states the rule — *"`schema_version` stays separate … The public
  API's optional `pos-api-version` header is a third, unrelated thing … Three axes, three names, never
  mixed"* — and `pos-edge-version` introduces no fourth. It publishes, on a second rail, the value
  `version.rs` already stamps from the release tag and `/healthz` already reports.
  ([`docs/naming-and-api.md`](../naming-and-api.md) §11 is the versioning section and names two of
  those axes, product version and `PROTOCOL_VERSION`; §4 is the HTTP API section, and it owns the
  header table this record adds a row to.)
- **The comparison is one-sided, and this record is what makes that safe.** The design assumes a
  *newer* edge never takes away a route an older app calls, so only "the app is ahead of its edge"
  needs checking. `AGENTS.md` §2's additive rule does not currently give that guarantee: it forbids
  removing or renaming a published **field, event, or permission** and says nothing about routes, and
  `docs/snapshots/` holds `capabilities.txt`, `events.txt` and `permissions.txt` — there is no route
  list to break. **So this record extends the rule: an edge `/api/*` route, once published, is not
  removed or renamed either; it is deprecated in place.** That is enforced the same way the other
  three are, by a snapshot — `docs/snapshots/routes.txt`, method and path per line, regenerated from
  the composed router and diff-checked in CI. Without it the one-sided comparison rests on a promise
  nothing keeps; with it, the app carries the minimum edge release it was built against and compares
  in that direction only.
- **`0.0.0` never warns.** [`version.rs`](../../crates/pos-edge/src/version.rs) makes `0.0.0` the
  honest answer for a hand-built binary, and it sorts below every real release. A developer running
  `just run-edge` against a shipped app should not see a banner on every call.
- **A behind-edge does not stop the till selling.** It shows a banner naming both versions and
  carries on. ADR-0024 already settled the principle for the tier below: *"a protocol mismatch
  degrades to 'not syncing', never to 'not selling'."* A version string is not a reason to refuse a
  customer. The banner is a user-visible string, so it ships as translation keys in every locale
  bundle like every other string in the app — `AGENTS.md` §2 forbids hardcoding one, and
  `ui/package.json`'s build runs `pnpm i18n:lint && pnpm i18n:parity` before `vite build`, so a
  hardcoded banner fails the build rather than the review.

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
being on its LAN. For a hosted edge placement it requires a script, and ten wrong codes from anywhere
shut
pairing for a minute — including for the operator standing at the counter holding a real code. That
is a denial-of-service lever the in-store deployment did not hand out.

**The budget is not re-keyed to fix it, and per-IP keying in particular is refused.** A hosted
edge placement sits behind a proxy, so the peer address is the proxy's unless a hop count is configured
correctly — `pos_cloud` already carries `trusted_proxy_hops` for exactly this and
`crates/pos-cloud/src/main.rs` warns that *"a wrong `trusted_proxy_hops` is a wrong rate-limit key"*.
And an attacker with an IPv6 allocation has a /64 to rotate through. A budget keyed on a value the
attacker chooses is not a budget; it is a counter that never reaches its limit while the operator's
single address does.

**`MAX_FAILED_REDEMPTIONS` and `REDEEM_LOCKOUT` stay compiled-in constants, not configuration.** A
tunable rate limit is a rate limit somebody widens at 19:30 on a Friday because "pairing is broken",
and never narrows again.

**The proxy carries the new load.** A hosted edge placement is fronted by a reverse proxy — it needs one
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
  unconditionally, and the `/ws` `Origin` check cannot refuse a till that the edge itself served,
  because the serving origin is derived from the request's own `Host` and needs nothing published.
  `pairing_url` is untouched, `advertised_ip` is untouched, the two auth gates are untouched, and
  `localStorage` remains the token store for every browser in every shop.
- **It does not put CORS on `pos_cloud`.** The console is served by `pos_cloud` and talks to it
  same-origin, exactly as the till does to the edge. There is no CORS in `pos-cloud` today and none
  is added here. If a second console front-end ever wants one, it is a different record with a
  different threat model — `/admin` carries a session cookie, which is precisely the combination this
  record refuses for the edge.
- **It does not give a hosted edge its TLS.** `https` in the pairing URL requires a certificate, and
  [ADR-0090](0090-tls-postures.md)'s four `TLS_MODE` values describe Caddy in front of `pos_cloud`,
  not `pos_edge`, which terminates nothing today. An `EDGE_PLACEMENT_HOSTED_BY_OPERATOR` store sits
  behind the operator's own proxy; an `EDGE_PLACEMENT_HOSTED_BY_PLATFORM` store sits behind
  [ADR-0113](0113-the-host-agent.md)'s, along with the hostname the public origin field would be set
  to.
- **It does not report pairing refusals to the fleet console.** The edge logs a shut-out with its
  failure count, and that is where it stays. Putting a refused-redemption counter in the heartbeat's
  optional JSON body ([ADR-0068](0068-fleet-liveness.md)) so the alert engine
  ([ADR-0073](0073-alerting.md)) can fire on a hosted store being probed is an obvious next step and
  is genuinely useful; it is not decided here, because nobody has yet said what threshold means
  anything, and an alert with a guessed threshold trains people to ignore alerts.
- **It does not build a shell, and it does not assume one works.** Tauri v2 against the real
  `ui/dist` on Android has not been spiked, and Android as an ESC/POS print agent has not been spiked
  either — both belong to [ADR-0112](0112-print-agents.md). Nothing above depends on either
  succeeding: if no native shell ever ships, this record still delivers the base URL, the origin
  allow-list and the version header, which is what a hosted edge placement and a second front-end
  need on their own.
- **It does not know the literal origin strings a native shell will use.** They depend on the shell
  and the platform, and the spike has not run. The node therefore validates the *shape* of an origin
  — an exact serialised origin, no path, not `null`, no wildcard — rather than an allow-list of
  schemes it would have to guess. That is a real piece of leniency and it is recorded here rather
  than hidden in the validator.

## Consequences

- `pos-edge` gains a CORS layer by enabling `tower-http`'s `cors` feature — a feature flag on a
  dependency already in the graph, not a new crate. One `CorsLayer` value is built in `compose`
  ([`server.rs`](../../crates/pos-edge/src/server.rs)) and passed to `http::router`,
  `http::domain_router` and `activation_router`, each of which applies it to its own covered subset
  and **outside** both auth middlewares, so a preflight is answered before `require_paired_device`
  refuses it. It is not applied to the merged application, because that would cover `/healthz`,
  `/ws`, the asset fallback and `POST /api/activate`.
- `AppState` ([`state.rs`](../../crates/pos-edge/src/state.rs)) gains an `Arc<Origins>` beside
  `pairing` and `fanout` — one `Mutex<Vec<Origin>>`, bounded at eight entries, replaced wholesale by
  the config-pull loop and read on the front of every request. `std::sync::Mutex`, the same as
  `Pairing`; no new dependency.
- The config tree gains an `origins` node on the Brand layer. The config-pull loop
  ([`config_client.rs`](../../crates/pos-edge/src/config_client.rs)) gains an `origins` branch
  **beside** `session_from_config` rather than inside it — `session_from_config` returns an
  `EdgeSession` for the application layer and the router cannot read one — parsing and validating the
  node under the never-blank rule and calling `Origins::replace`. The cloud gains a publish route of the
  shape every other config node already uses: behind
  `ConsolePermission::PublishConfig` ([ADR-0067](0067-multi-admin-console-rbac.md)), with an audit
  entry naming the acting admin ([ADR-0069](0069-audit-trail.md)), and — like its eleven siblings —
  **no `If-Match`**. That is not an omission. `publish_config_nodes`
  ([`http.rs`](../../crates/pos-cloud/src/http.rs)) takes no `HeaderMap`, so it could not read one,
  and the code gives the reason: *"a node publish sets one key and commutes with the others, so it
  retries."* Conditional writes on the config tree are `PUT /admin/config` and the version restore
  ([ADR-0095](0095-conditional-writes-for-collections.md)), not a node publish. `origins` is a node, so
  it takes the node's contract rather than inventing a twelfth.
- **`origins` makes ADR-0033's deferred fan-out cost visible.** Its value is identical for every store
  under a brand, and ADR-0033 records that *"a shared Tenant/Brand layer that fans out to every store
  under it is a future modeling step; today each store's tree holds its own four layers."* So
  shipping a new app shell to 500 stores is 500 publishes. This record does not fix that; it is the
  first node for which the missing fan-out is the dominant cost rather than a tidiness complaint.
- Every covered route now answers `OPTIONS`. That is twenty-six new method/route pairs in the
  router's surface, and **there is no test that asserts the edge's route list today** —
  `tests/http.rs` asserts `/healthz` and that the UI is served, `tests/acceptance.rs` drives the
  composed surface for behaviour, and `docs/snapshots/` holds only `capabilities.txt`, `events.txt`
  and `permissions.txt`. This change adds `docs/snapshots/routes.txt`, generated from the composed
  router and diff-checked in CI like the other three, because the version handshake's one-sided
  comparison depends on routes being additive and nothing enforces that today.
- [`docs/naming-and-api.md`](../naming-and-api.md) §4's header table gains a `pos-edge-version` row in
  the same change as the client code that reads it. The table is described there as the contract and
  the code is checked against it; a header with no reader has been removed from that table once
  already.
- [`ui/src/api/client.ts`](../../ui/src/api/client.ts)'s opening comment stops being true and is
  rewritten in the same pull request. The file gains a base-URL accessor defaulting to `""` and a
  token-store seam. **All three of its `fetch` call sites take the base**, not only `request()`:
  `signIn` and `signOut` call `fetch` directly so they can read a structured refusal body, and a
  shell that could settle a bill but never sign anyone in is not a shell.
  [`live.ts`](../../ui/src/api/live.ts) derives its socket URL from the base instead of
  `window.location`.
- `request()` also gains the one thing the drift problem needs from it: a response whose status is
  `ok` but whose content type is not JSON is a missing route, not a value, and it fails as a named
  error carrying the `pos-edge-version` header rather than as a bare `SyntaxError`.
- `EdgeConfig` gains an optional public-origin field beside `advertised_ip`, and `pairing.rs` gains a
  second URL constructor. `pairing_url` itself, its `IpAddr` parameter and its test
  (`the_pairing_url_carries_the_code_over_raw_ip`) are unchanged.
- `SecretName` gains one variant for a device token — additive on a `#[non_exhaustive]` enum, and the
  first entry in it that names a secret held by a selling device rather than by a store server or a
  cloud deployment. `SecretName::ALL` is what a wipe-on-revocation routine *would* iterate, and the
  port's own doc comment is careful to say "a" rather than "the": **`ALL` has no production caller
  today.** Its only readers are the port's unit test in
  [`key_vault.rs`](../../crates/pos-ports/src/key_vault.rs) and the shared contract suite in
  [`pos-contract-tests`](../../crates/pos-contract-tests/src/key_vault.rs). So the new variant is
  covered by the *contract* for a future sweep, not by a sweep that runs — and this record does not
  build the sweep.
- **The test matrix gains a third axis.** ADR-0110 already doubled it by `edge_placement`. Every covered
  route now also has a same-origin case, an allow-listed cross-origin case with its preflight, and a
  refused-origin case — and the refused case must assert that the *response* is refused by the
  browser rule, not that the handler ran differently, because the handler does not know.
- **The additive rule now names routes.** `AGENTS.md` §2 covers published fields, events and
  permissions; this record extends it to edge `/api/*` routes, enforced by `docs/snapshots/routes.txt`.
  A route is deprecated in place, never removed or renamed.
- Nothing is removed and nothing is renamed. `PROTOCOL_VERSION` does not move: the app is not a party
  to the edge↔cloud protocol, and a response header, a config node, an optional config field, a route
  snapshot and a `SecretName` variant are all additions. An edge published no `origins` node behaves
  exactly as this fleet's 500 stores behave today.
