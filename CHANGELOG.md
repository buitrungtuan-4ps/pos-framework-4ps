# Changelog

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

All notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning follows [Semantic Versioning](https://semver.org/) for the product and a separate `PROTOCOL_VERSION` for the cloud–edge wire format (see [`docs/naming-and-api.md`](docs/naming-and-api.md) §11).

**Rules for entries**

1. Every user-visible change gets an entry, written for the reader, not the author.
2. Categories: `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security`.
3. Reference the issue or pull request number.
4. Add an **Upgrade note** whenever the change affects `PROTOCOL_VERSION`, a migration, a permission identifier, or a default value.
5. Nothing is ever removed without having been deprecated for at least two releases.

---

## [Unreleased]

### Added

- **The console has a landing page that answers "is this shop all right"** (roadmap v3 **Q4**,
  [ADR-0099](docs/adr/0099-store-hub.md)). Q4's URL half shipped long ago — a tenant is a path
  segment and the store a `?store=` query, so a console link is shareable — and the screen it was
  *for* was never built, which is why the slice read as done.

  The tenant-scoped index now renders a six-card **store overview**: whether the box is online and
  when it last checked in, whether it holds the configuration that was published to it, what it took
  on its latest trading day, how many tills are open, how many items are out of stock, and what is
  firing against it. Each card links to the screen that can act on the answer; the hub itself is
  read-only, because a hub that writes is a second copy of five editors.

  **Reports moves to `/reports`** and keeps every capability. It is a good screen and it was the
  wrong *first* screen: it answered how much the shop made before saying whether the shop was
  online. A bookmark of `/t/<tenant>?store=X` now opens the overview; nothing 404s.

  **Two cards are honest approximations and say so on the card.** "Shifts open" is a count of tills,
  not a list of names — the cloud projects no roster, and a roster would be employee personal data
  needing a lawful basis rather than a card. "Out of stock" is the day's net count, not the live
  list — the events are counted but no projection folds them into "which dishes". Both are recorded
  as follow-ups in the ADR instead of being dressed up. Takings is the only card behind a permission,
  because prices are T2; a shift count and an out-of-stock count are operational facts, and gating
  them behind the revenue permission would have left an Ops admin unable to see that the kitchen had
  run out of something.

  No new route, projection, migration or permission: the hub is composed from four reads the console
  already made.

### Fixed

- **Every store the console has ever provisioned published none of its sales** (roadmap v3 **E3**).
  The new-store wizard's `config.toml` generator emitted no `[nats]` section, and
  `spawn_event_publish` returns early without one — logging that the outbox is not being published
  and then trading normally. So a box brought online exactly as the runbook says committed every
  sale locally, durably, and shipped nothing: the cloud's rollups, reports and reconciliation all
  read a stream nothing published to. Nothing surfaced it, because a store that publishes nothing
  looks exactly like a store the cloud has not heard from yet.

  The generator now emits the section, and the environment file's `POS_EDGE_NATS_URL` template is
  the shape that works — `tls://:<token>@<your cloud host>:4222`. It previously read `nats://`,
  which connects in plaintext and is refused by the broker E7 put TLS on, against a placeholder
  host. It stays **commented**: the broker token is one secret for the whole fleet, held on the
  cloud box, unlike the per-store key the wizard does emit, so the console will not put it in a
  browser and then into every machine in the estate. The generated file carries the one command
  that recovers it. A `[nats]` section with no URL logs a warning naming the missing variable —
  configured, not yet armed, which is the truth.

  **The values are fleet-wide, and that was a decision, not a default** ([ADR-0087](docs/adr/0087-edge-relay-and-event-publish.md)
  Amendment 1). The tree carried two incompatible conventions: three doc comments said one stream
  per store (`POS_STORE_<id>`), while `bootstrap.sh` documents arming the cloud with `POS_FLEET` and
  `pos_cloud` binds exactly **one** durable consumer to **one** named stream — so a fleet of
  per-store streams would have been ingested one store deep. Every store now publishes into
  `POS_FLEET` on `pos.fleet.events`, the same on every box, matching `cloud.toml`. The shared
  *subject* is the load-bearing half: the edge's handshake is a create-or-get, which does not add a
  subject to a stream that already exists, so a per-store subject inside a shared stream would have
  been captured for the first box to connect and refused for every one after it — a failure that
  appears only on store number two, in production, long after the console was tested against one
  shop.

  **Upgrade note.** A store already in the field needs the `[nats]` table added to its `config.toml`
  and `POS_EDGE_NATS_URL` set in `/etc/pos-edge/env`; re-downloading the two files from the store's
  wizard produces both. Nothing is lost in the meantime — the outbox is durable and drains from the
  beginning once the stream is reachable. `NATS_MAX_MESSAGES` (1 000 000) and `NATS_MAX_BYTES`
  (1 GiB) are now a **fleet** ceiling rather than a per-store one; reaching either refuses new events
  rather than dropping old ones, so it shows up as the 80% capacity alert and a growing outbox, not
  as loss. Sizing them against a real estate is the A·P4 **O4** capacity probe.

- **A tip can now actually be taken, and a store's refused tender is no longer offered** (roadmap v3
  **B1.3** and the **E5** residual). The domain halves of both shipped in #181 and neither could
  fire.

  **Tips.** `Payment.tip` reached the edge, `decide_bill` required `Capability::Tips`, and the
  settled event recorded the amount — but the till had **no tip entry anywhere**, so `tip_amount`
  was zero on every payment a real store took, whatever the capability said. The Pay and counter
  screens now carry a tip row (no tip · 5% · 10% · 15%), shown only when the store takes tips.

  The tip comes out of the **change**, not out of the sale: the quick-cash keys, the exact-amount
  default and the change on screen are all computed against sale + tip, so the figure the cashier
  reads is the one the edge records. B1.3's second defect was exactly this subtraction missing on
  the edge — a till telling a cashier to hand back money the guest had just left — and getting it
  wrong on the screen would have been the same lie from the other side.

  **Tender.** No route serialized the store's `accepted_tender`, so a cash-only store still showed a
  Card button and the edge's refusal landed as a `400` in front of the guest. Both pay screens now
  hide a method the store does not accept. An unrestricted store (`null`, not an empty list) keeps
  accepting everything, including a method added to the enum later.

  **And the fifth hardcoded `VND`.** `ui/src/screens/Takeaway.tsx` still offered VND's three
  banknotes on any currency — the same defect fixed one file over in `Pay.tsx`, written the same way
  and missed. E5 named three sites, #181 said four; there were five.

### Changed

- **`docs/ui-ux.md` §6: the step budget governs the steps a task *requires*.** An optional
  enhancement — a tip on a settle — is declared as its own task with its own ceiling rather than
  counted against the flow it sits inside. Without the distinction the rule pushes an optional
  control behind a button to keep a number down, which costs a tap in the case the operator wants it
  and buys nothing in the case they do not. The two tipped settles are declared at **four** on that
  basis, each with its reason in its own note; `ui/scripts/step-budget.mjs` now resolves 15 tasks
  and 27 taps.

### Added

- **The till is told whether its store takes tips, and which tender it accepts** (roadmap v3
  **B1.3** and **E5**, `GET /api/menu`).

  Both facts were already live in `EdgeSession`, applied from published configuration, and read by
  **nobody**. The consequences were not symmetric.

  `accepted_tender` made the till offer a method the edge would refuse, so a cash-only store's
  refusal arrived as a `400` in front of the guest instead of a button that was never there.

  Tips were worse than a bad refusal. B1.3 landed the domain, the event and the wire — a `tip` on
  each `Payment`, gated on `Capability::Tips`, with the change arithmetic corrected — and shipped no
  way for a cashier to enter one. There is no tip pad, no suggested-percentage row and no i18n key
  anywhere in `ui/`, so **`tip_amount` is zero on every payment a real store takes**, whatever the
  capability says. This entry is the first half of closing that: the till can now *know*. The pad
  itself is the next commit.

  `MenuResponse` carries them because they are the same kind of fact as a price — published from the
  console, resolved for this store, refreshed when the price book is — and because the till already
  reads that route on load, so nothing new is called or authorised. `GET /api/session` was the other
  candidate and is the wrong one: it answers *who is signed in on this device*, per-device identity
  with a per-sign-in lifetime, and hanging store-wide configuration off it would conflate the two.

  **Two flags, not ten.** The session carries ten capability flags and the till could be handed all
  of them; nine would arrive with no reader, which is the failure this repository has shipped
  repeatedly (`docs/roadmap-v3.md` Cadence). A flag joins this response in the change that consumes
  it; B5.3 is where the rest arrive with their gates.

  `accepted_tender` is `null` rather than a list of all seven methods when a store restricts
  nothing, so the till can tell "no restriction published" from "a restriction that happens to allow
  everything" — and so a method added to the enum later is accepted by an unrestricted store without
  a config change.

  Verified through the composed router rather than by calling the handler, because the question is
  whether the shipped binary serves them, and in both directions: a store with tips **off** says so.
  Reinstating the bug fails the test with `left: Bool(false), right: Bool(true)`.

- **`pos_edge` runs as a real Windows service** (roadmap v3 **E4**,
  `crates/pos-edge/src/service.rs`). Two things were wrong, and the second is the one that mattered.

  **The documented install could not work.** `deploy/edge/README.md` said to run `sc.exe create` and
  `sc.exe start`; the Service Control Manager gives a starting service about thirty seconds to
  connect back and report `RUNNING`, and a plain console program never does, so SCM kills it with
  *error 1053*. Every Windows store was therefore either under a third-party shim or running in a
  console window — which nothing restarts after a power cut. The binary now speaks SCM's protocol
  itself: started as a service it reports `RUNNING`, a stop (and a machine shutdown) drains in-flight
  requests exactly as a `SIGTERM` does on Linux, and started from a console it notices SCM is absent
  and runs in the foreground, so `--self-test` and an operator's manual run are unchanged.

  **Over-the-air updates could not have worked on Windows even with a shim.** An install retargets
  `bin\current` and exits, and on Linux `Restart=always` turns that exit into a start on the new
  binary. SCM has no such setting — it has failure actions, applied only when a service *looks* like
  it failed, and a service that reports `STOPPED` with exit code zero has been stopped on purpose.
  A store that installed a release at 11:40 on a Saturday would have sat dark until somebody drove to
  the shop. So `serve` now returns a **`ServeOutcome`** — `Stopped`, or `RestartWanted` — and the
  wrapper reports exit code `1` for the second, which the documented
  `sc.exe failure … actions= restart/5000` acts on. An operator's own `sc.exe stop` still exits zero
  and stays stopped.

  `deploy/edge/README.md` now carries the five registration commands, says why the `failure` line is
  not optional, and records what the symlink privilege needs (Administrators and `LocalSystem` hold
  it; a named low-privilege account does not, and the edge reports the absence rather than failing an
  install every ten minutes). One new dependency, `windows-service`, target-gated so a Linux build
  compiles none of it.

  **Upgrade note.** A Windows store already registered with `sc.exe` should add the failure action —
  without it the box will not come back from an update. Nothing changes for a Linux store, where the
  two stops were always indistinguishable and always restarted. There is no wire, migration,
  permission or default change.

  What is not proven: SCM's own behaviour. The `windows-2022` CI job compiles the wrapper, so a
  rename or a signature change fails a pull request, but that a real service reaches `RUNNING`,
  drains on stop and restarts on exit `1` needs a Windows box —
  [`docs/gate-register.md`](docs/gate-register.md) P3, which this narrows rather than closes.

- **A per-store installer on the new-store wizard's handoff step** (roadmap v3 **R3**).
  The wizard already handed the operator a `config.toml` and an `env` file and then asked them to
  follow a README on the shop floor: create a service user, get two sets of owners and modes right,
  install a systemd unit, and lay out the update slots. **Download installer** now produces
  `install-pos-edge.sh` for that one store, carrying both files inside it as quoted heredocs. Run it
  as `sudo sh install-pos-edge.sh ./pos-edge ./pos-edge.service` and the box is laid out, enabled and
  started.

  The slot layout is the reason it is worth generating rather than typing. Since ADR-0055
  Amendment 1 the unit starts `/var/lib/pos-edge/bin/current`, a symlink the edge retargets to
  install its own updates; a box with the binary only at `/usr/local/bin/pos-edge` trades perfectly
  well and **silently never self-updates** — the kind of mistake a hand-typed install makes and
  nobody notices for a release or two.

  A re-run refreshes the config, the unit and the rescue copy but **leaves an already-installed
  binary alone**: a box that had updated itself over the air would otherwise have `current` pointed
  back at whatever binary the technician was holding, which is a silent downgrade of a live shop.
  That guard is also what makes the claimed idempotency true.

  It is not a `curl | sh`: nothing in it reaches the network, and the operator can read every line
  before running it. It **contains the store's key**, so the handoff screen says so in as many words
  and the script's own header says to delete it once the box is up. The two separate downloads stay
  for the cases the script cannot cover — a Windows box, a host managed some other way, or a
  technician who wants to read the config before it is written.
  [`docs/guides/bring-a-store-online.md`](docs/guides/bring-a-store-online.md) Step 2 now leads with
  the installer and keeps the manual path beside it.

- **The UX step budget is now measured** (roadmap v3 **Q7**, `docs/ui-ux.md` §6).
  `docs/ui-ux.md` has said since P6 that a common action takes at most two taps from the role's home
  screen and a rare one at most three — the rule behind design principle 1, "a normal operator sells
  without training" — and nothing checked it. A rule nothing measures decays one convenient extra
  dialog at a time.

  `ui/scripts/step-budget.mjs` declares the tap count of **thirteen** selling tasks and resolves
  every declared tap against the source: the route must exist in `App.tsx`, its screen must exist,
  and an interactive element on that screen must really call the named action. `pnpm build` runs it,
  so the `ui` CI job fails on a breach. Zero new dependencies — it uses the TypeScript compiler the
  i18n gate already uses. Run alone with `pnpm steps`; it prints every task and marks the four
  sitting **at** their ceiling, which are the flows to defend hardest (cash settle, both counter
  charges, the kitchen bump).

  All three of its failure modes were verified by breaking them: a renamed handler, a route that does
  not exist, and a flow one tap over budget.

  **What it cannot see is stated in the script and in the doc**: a required tap nobody declared. Add
  a confirm dialog to the pay flow and leave the declaration alone, and the gate stays green while
  the flow is one tap worse. Catching that needs a browser driving a running edge with a paired
  device — a harness this repo does not have, filed with what it would take. So the gate stops a
  budget being quietly raised and a flow being quietly renamed; a reviewer still owns "did this add a
  step?", and the printed map is what makes that a five-second question.

- **An integrator guide** (roadmap v3 **Q6**): `docs/guides/integrate-with-the-api.md`, for a third
  party connecting to the cloud rather than for someone running it. Three parts — authentication and
  what a refusal means, a tour of what `/v1` actually offers, and a webhook quickstart with a
  complete verifying receiver.

  Written now rather than earlier because it can finally be *correct*: the webhook signature headers
  it documents are the ones the cloud sends only as of this release, and the surface it describes no
  longer includes the event feed and version pin that the standard advertised but nothing built. It
  says plainly what does not exist — no public event feed, no `pos-api-version`, no writes beyond the
  order intake — because a guide that omits the absences sends integrators looking for them.

  Three things it is explicit about because getting them wrong is expensive: a `201` on an order
  means the store accepted it, **not** that the kitchen has started; a `503` calls for the look-up
  rather than a blind resubmit; and a webhook delivery is a **page** re-sent until accepted, so a
  receiver must verify over raw bytes, check the ±5-minute replay window, and dedupe on each event's
  own `event_id`. It also states that a line's `note` is personal data and what may and may not go
  in it.

- **`/v1/orders` and `/sync/*` now have a budget** (roadmap v3 **Q5**). Both were unbounded: every
  call authenticated, resolved a store and reached storage, so a caller in a retry loop cost the
  cloud that work per iteration with no ceiling. The shape is not hypothetical — the fork checklist
  already documents a store issued the wrong scope answering `403` **on every poll, every five
  seconds**, indefinitely.

  The sliding-window limiter written for `/admin/login` was already generic over an opaque key, so
  it is reused rather than reinvented (and renamed from `LoginRateLimiter`, since it now serves
  three surfaces). Each surface holds its **own** counter, so a store hammering `/sync` cannot lock
  an admin out of the console — there is a test for exactly that.

  The two surfaces are keyed differently, on purpose:
  - **`/v1/orders`** by the caller's **tenant**, after authentication. The intake is shared between
    integrators, so what is worth preventing is one marketplace's runaway loop consuming the
    capacity the others need, and the tenant is the identity the bearer key proves. The look-up
    (`GET /v1/orders`) spends the same budget as the submit: it is the path a caller retries when a
    submit times out, so exempting it would leave the loop that most needs bounding unbounded.
  - **`/sync/*`** by the **client connection**, before authentication — as a layer over the whole
    composed service, since the store-facing surface spans more than one sub-router. Per-store would
    read better and is deliberately not used: the store id sits in the caller-supplied path, so
    keying on it would let anyone exhaust a named shop's budget by spelling its id, which is a
    targeted denial of service on one restaurant. Checking before the credential is verified is the
    point — a wedged box should cost a header comparison, not a key lookup and a database round trip.

  A refusal is the usual AIP-193 `RESOURCE_EXHAUSTED` with a `Retry-After`, through one helper every
  throttled surface now shares.

  **Upgrade note:** four new optional `CloudConfig` keys, all defaulted, so an existing config file
  boots unchanged: `orders_max_requests` (300) and `orders_window_secs` (60) — five orders a second
  for a whole integrator; `sync_max_requests` (600) and `sync_window_secs` (60) — ten requests a
  second per connection, roughly fifty times what a healthy store generates. Both are sized to bound
  a runaway loop rather than to shape normal traffic; a deployment with unusual integrators can
  raise them. Limiter state is in-process and ephemeral, so a restart clears it — which fails open,
  never closed.

### Removed

- **The two API-key scopes that gated nothing** (roadmap v3 **Q5**). `read_events` and
  `manage_webhooks` were variants of the cloud's `Scope` enum and **no route consulted either**.
  The console's picker had already stopped offering them, but `POST /admin/api-keys` still
  **accepted** them — so an integrator could be handed a key whose scope list promised an authority
  the cloud would never honour, with nothing anywhere to say so. Deleting the variants is what turns
  that into a refusal: the strict parse on the issue route now names the unknown scope back.

  Wiring them instead was considered and rejected as out of scope for a tail item. `read_events`
  named a public event feed that does not exist — a new read surface with its own PII, retention and
  paging decisions — and `docs/naming-and-api.md` used `GET /v1/events` as its pagination example,
  which is now corrected to say plainly that the route was never built and how events do leave the
  cloud. `manage_webhooks` named webhook CRUD on `/v1` behind a bearer key, while the console
  already has that CRUD behind a session and RBAC ([ADR-0067](docs/adr/0067-multi-admin-console-rbac.md));
  a second path would be weaker auth on the same writes.

  **Upgrade note:** a key already stored with either name keeps working and authorises exactly what
  it did before — unknown scope names are dropped on read (deny-by-default), and these two granted
  nothing to drop. What changes is issuing: a request naming either scope is refused rather than
  silently satisfied. No migration; no stored data is rewritten. `docs/fork-checklist.md` records
  what a fork would have to build to have either capability.

### Fixed

- **The published webhook headers were not the ones the cloud sent** (roadmap v3 **Q5**,
  [ADR-0032](docs/adr/0032-webhooks.md)). `docs/naming-and-api.md` §4 is the table an integrator
  builds a receiver from, and it listed `pos-signature` / `pos-signature-time` while the sender sent
  `X-Pos-Webhook-Signature` / `X-Pos-Webhook-Timestamp`. A receiver written from the document
  therefore found no signature and rejected **every** delivery as unsigned.

  The code now sends the published names, which also drops the `X-` prefix
  [RFC 6648](https://www.rfc-editor.org/rfc/rfc6648) deprecated. A test asserts both names as
  literals, so changing either one fails until the published table changes in the same commit.

  The table was wrong in the other direction too, and that half is fixed by correcting the table:
  - `pos-event-id` and `pos-delivery-id` described a webhook that delivers **one event**. It
    delivers a **page**, re-sent unchanged until the receiver accepts it, so there is no single
    event to name and no delivery record with an id. Both rows are gone, and the document now says
    what a receiver dedupes on instead: each event's own `event_id`, inside the body. A per-attempt
    id is a real addition rather than a fix and is flagged as follow-up work, not smuggled in here.
  - `pos-api-version` was an optional minor-version pin that **no route read**. An integrator who
    sent it believed they had pinned a version and had not, which is worse than no header. It is
    removed from the table and from `pos-proto`'s "three numbers" note, which is now two:
    `PROTOCOL_VERSION` and per-event `schema_version`.

  **Upgrade note:** a webhook receiver reading the old `X-Pos-Webhook-*` headers stops seeing them
  and must read `pos-signature` / `pos-signature-time`. Nothing else about the delivery changes —
  same `v1=<hex>` HMAC-SHA256 over the same body, same ±5-minute replay window, same signing
  secret — so a receiver that reads the header name from configuration needs only that value
  changed. The edge↔cloud `X-Pos-*` headers are deliberately untouched: both sides must agree on
  those, and they are not a published contract.

- **The till assumed VND in four places** (roadmap v3 **E5 residual**,
  [ADR-0074](docs/adr/0074-localization-and-tax.md)). The edge has always known the store's currency
  — it comes from the synced `locale` node and the edge already served it on `GET /api/menu` — but
  the app discarded that field and wrote `"VND"` as a literal instead. On a store outside Vietnam:
  - the shift's **opening float** was *sent* to the edge tagged `VND`, so a store's own cash count
    was recorded in a currency it does not trade in;
  - the typed float and count were **parsed at VND's scale** (no minor units), which is out by a
    factor of a hundred on any two-decimal currency;
  - the shift screen's expected and counted figures fell back to a zero labelled `VND`;
  - the pay pad's quick-cash keys were VND's three banknotes (50k/100k/200k), offering amounts the
    store's guests cannot hand over.

  All four now read the currency the edge reported. Banknote values moved next to the minor-unit
  table in `ui/src/lib/money.ts`, since they are a property of a currency rather than of a screen; a
  currency with no note table offers the exact amount alone, which is always tenderable, rather than
  guessing denominations. The pay pad keys on *the bill's* currency, as every other figure on that
  screen already did.

  **Upgrade note:** none required. A Vietnamese store behaves identically — the edge reports `VND`
  and the note table's first row is the same three notes. `DEFAULT_CURRENCY` in the app's store is
  the never-blank fallback for the moment before the price book loads, in keeping with the existing
  `DEFAULT_FLOOR`/`DEFAULT_STATION`; a fork changes that one constant.

- **Tips were recorded as zero, and change was over-reported by the tip** (roadmap v3 **B1.3**,
  [ADR-0028](docs/adr/0028-settlement-and-payment-invariant.md)). `billing.payment.captured` has
  carried a `tip_amount` since P5 and it was written as a literal zero on every payment ever
  captured — so a store could take tips all night and its own log said nobody tipped, and tip-out
  could not be reconstructed from it.

  The cause was the shape, not the assignment. Tips travelled as a `Vec<Money>` **beside** the
  payments, with no correspondence between the two lists. The settlement arithmetic came out right
  in total — tips were correctly excluded from `total_due` and correctly subtracted from the total
  change — but nothing knew *which* tender a tip was taken on, so no captured payment had a tip to
  record. The tip now lives on `pos_core::billing::Payment` and the parallel list is gone.

  That shape hid a second, worse defect. Each payment's `change_given` was computed as
  `tendered − applied_to_bill`, which over-reports the change by exactly the tip: a guest handing
  over 200,000 on a 165,000 bill and leaving 20,000 was recorded as owed **35,000** back rather
  than 15,000. The arithmetic now lives in one place, `Payment::change`, which subtracts the tip.

  A tip also now requires the store to have tips on (`tips_enabled`, §10). The gate refuses taking
  the money, never the sale: a store with tips off settles untipped bills exactly as before, and a
  tipped tender is refused rather than pocketed silently.

  **Upgrade note:** `POST /api/bills/{id}/settle` takes the tip per payment
  (`payments[].tip`, optional) instead of a request-level `tips` array. A device that sends neither
  settles exactly as before; one that still sends `tips` has that field ignored, so its tips would
  go unrecorded — update the caller. `Payment`, `billing::settle`, `BillCommand::Settle` and
  `Edge::settle_bill` changed shape with it. No migration, no event-schema change (the event field
  already existed) and no `PROTOCOL_VERSION` change: the fix is that the field is now populated.

- **Every order was taxed at the store's channel, not its own** (roadmap v3 **B1.2**,
  [ADR-0074](docs/adr/0074-localization-and-tax.md)). Tax rates are configured per tax class **per
  channel**, and both bill reads looked the rate up with `EdgeSession.sales_channel` — a store-wide
  constant. A store that sells dine-in *and* takeaway therefore charged one rate for everything: a
  counter order settled on a store whose session said dine-in got dine-in tax. Nothing failed and
  nothing logged; the only symptom was a wrong figure on a receipt, on every order that did not
  arrive through the session's channel, for as long as the store traded.

  `sales.order.opened` has carried a `channel` since P5 and nothing ever read it back, because there
  was nowhere to read it into. Three things changed:
  - The edge projection keeps each order's channel, folded from `sales.order.opened` on replay as
    well as recorded live, so the rate survives a restart between an order being placed and its bill
    being settled.
  - `Edge::seat_table` now emits `sales.order.opened` alongside `sales.table.opened`, in one
    transaction. It previously emitted only the table event, so a dine-in order existed with no
    record of the channel it came in on at all.
  - `order_totals` and `settle_bill` share one `taxable_bases` read that returns the bases and the
    channel together, so the two paths cannot resolve the channel differently — which is how the
    defect was written in the first place.

  An order whose channel is not knowable — one opened before this change, or carrying a token this
  build does not understand — falls back to the session channel, exactly the old behaviour and the
  only answer available for a sale already in the ground. A newer sender's unrecognised channel is
  recorded as "no answer" rather than failing the replay: a box that cannot rebuild cannot trade.

  `POST /api/tables/{id}/seat` also accepts an optional `guest_count`, which
  `sales.order.opened` has always carried. No screen prompts for it yet — whether tap-to-seat should
  grow a step is roadmap **Q7**'s call — but the route carries it so that decision needs no wire
  change. The body stays optional in both spellings devices use: absent, or `application/json` with
  nothing after it.

  **Upgrade note:** the bootstrap tax table now carries the standard class at 10% for **every**
  channel rather than dine-in alone. A store's arithmetic is unchanged (the same 10% across the
  board), but each cell now exists, because assembling a bill refuses a class with no rate for the
  order's channel rather than papering over it with zero tax — and without the added rows a LAN-only
  store could not settle a takeaway order it had already accepted, priced and fired. A store with
  cloud-published rates is unaffected; those still override. No migration and no `PROTOCOL_VERSION`
  change.

- **A fired line consumed only its base recipe, never its modifiers** (roadmap v3 **B1.1**,
  `docs/pos-spec.md` §8). `fire_line` passed `modifiers: Vec::new()` to `decide_line`, so a large
  pizza deducted the base dough and never the "large" modifier's extra 50 g. Nothing failed and
  nothing logged: the shelf simply disagreed with the books by exactly the modifiers, on every
  fired line.

  The ids had nowhere to travel, which is why the argument was empty. Three places carry them now:
  `sales.order_line.added` gains `modifier_menu_item_ids` (additive, `#[serde(default)]` so a store
  upgrading can still replay its own log), `pos_core::menu::PricedLine` carries them through
  repricing instead of dropping them once their prices were summed, and the edge's line record keeps
  them from add to fire. `POST /api/tables/{id}/lines` accepts them; a device that sends none behaves
  exactly as before.

  Also fixed in the same seam: the consumption `decide_line` computed was **discarded**. The one
  place §8's arithmetic ran had no reader at all. It now folds into a `StockProjection` on the edge.
  That figure is a running consumption total, not an inventory — nothing seeds it with what the store
  received, so it reads negative until the flagged goods-in/stocktake slice lands, and nothing 86s a
  menu from it (`EdgeSession::item_sellable` still has no production caller). What the fold buys
  today is that the arithmetic is real and testable rather than thrown away.

  **Upgrade note:** none required. The event field defaults, the route field is optional, and no
  migration or `PROTOCOL_VERSION` change is involved. A store that upgrades keeps its history; lines
  added before the upgrade replay with no modifiers, which is what they were recorded as.

### Added

- **Over-the-air updates run on the shipped edge** (roadmap v3 **R4** + **R5-d**,
  [ADR-0055](docs/adr/0055-edge-ota-updater.md) Amendment 1). The rollout decision, signature
  verification and install orchestration have been merged and tested for four releases and **nothing
  ran them**: `OtaUpdater` had no caller outside a test, and there was no installer for any operating
  system. Both halves land together, because an installer with no caller is the same gap again.
  - `crates/pos-edge/src/installer.rs` — the Linux install seam. Two version slots under the
    service's own state directory, promoted by one atomic `rename(2)` of the `bin/current` symlink,
    with the `.pre-update` database copy taken before anything is written and `bin/previous` recorded
    before the swap so a power cut leaves a box that still runs the old binary and knows where to go
    back to.
  - `crates/pos-edge/src/ota_client.rs` — the loop. It reads the published `fleet_update` and
    `device_ota` nodes out of the live session, assembles the device state through
    `ota_state::device_state`, weighs the rollout, and reports the outcome to the cloud before asking
    the process to drain so the service manager restarts it into the new binary.
  - `pos-edge --self-test` — the pre-commit smoke test, run against the *staged* file before
    anything is swapped: can these bytes run on this box, and do they read this store's
    configuration. It catches the wrong architecture and a truncated download; it deliberately does
    not open the database, which the running edge still owns.
  - A **self-healing revert.** A committed version is marked unconfirmed until it reaches a healthy
    boot; past three attempts the edge points `current` back at the previous version, restores the
    database and exits. A release that crash-loops can never record its own verdict, which is
    exactly why the marker exists — without it a bad release needs somebody to drive to the shop.

### Changed

- **`deploy/edge/pos-edge.service` runs the binary from the state directory.** `ExecStart` is now
  `/var/lib/pos-edge/bin/current`, a symlink the edge owns, rather than `/usr/local/bin/pos-edge`.
  The unit's `ProtectSystem=strict` and `NoNewPrivileges` make everything outside the state directory
  read-only to the process — deliberately — so a self-update cannot write `/usr/local/bin` and must
  not be given the privilege to. `/usr/local/bin/pos-edge` stays as the operator's rescue copy.

  **Upgrade note.** An existing store keeps trading unchanged and simply does not self-update: with
  no `bin/current` the edge logs that it found no layout and starts no updater. To turn updates on,
  follow the install block in the unit's header (create `bin/`, copy the running binary to `slot-a`,
  link `current` at it, change `ExecStart`) and reload systemd. `Restart=always` becomes
  load-bearing rather than merely prudent — a clean exit is how the edge asks to be restarted into a
  version it just installed, so `on-failure` would leave a store stopped after an update.
- **`FakeCloudSync`'s release fixture lost its leading `v`, and its artifact became a runnable
  program.** `KNOWN_RELEASE` was `v1.2.3` while a rollout publishes, and a binary reports, the bare
  `1.2.3` — so the first real caller asking the fake for a release got "no such release", and the
  fake would have hidden the very drift [ADR-0088](docs/adr/0088-ota-artifact-hosting.md)
  Amendment 2 exists to prevent. And the artifact bytes were a placeholder string, which the real
  installer's smoke test cannot execute: every install-path test would have come back a rollback,
  proving that a truncated download is caught rather than that a good release installs.

- **The two OTA routes a store calls moved from `/internal` to `/sync`, so a store can actually reach
  them** (roadmap `docs/roadmap-v3.md` **R5**, [ADR-0088](docs/adr/0088-ota-artifact-hosting.md)
  Amendment 1 and [ADR-0097](docs/adr/0097-internal-route-authentication.md)'s delivery note). The
  reverse proxy answers `404` to the whole `/internal` prefix from outside the box, so both the
  artifact fetch and the update report were unreachable by the only caller they have — the same defect
  #175 found for the artifact route, present twice.

  `POST /sync/stores/{store_id}/report` is new: the tenant comes from the scoped key and the store
  from the path, so a report is **tenant-attributable**, which ADR-0097 recorded that no strength of
  shared secret could make the `/internal` shape. It is not store-attributable — a grant pins a
  tenant, not a store — and that residual is stated in the ADR rather than glossed. The artifact fetch
  now sends `arch`, stamped into the binary from Cargo's own `TARGET` by `build.rs`, because guessing
  it from `std::env::consts` is silently wrong for a musl fork.

  `POST /internal/ota/report` still exists and keeps its shared-secret guard for on-box tooling, but
  the edge no longer posts to it. Both routes share one write helper, so the only difference between
  them is where identity comes from.

  **Upgrade note** — no `PROTOCOL_VERSION` change and no migration. The edge and the cloud ship in the
  same release, so no store speaks the old wire to a new cloud or the reverse. `arch` is additive.
  A fork that calls `/internal/ota/report` from its own cloud-side tooling is unaffected; a fork that
  somehow had a *store* calling it was getting a proxy `404` and now works.

### Added
- **A release can be put into the cloud, and cannot be promoted before it is** (roadmap `docs/roadmap-v3.md`
  **R2**, [ADR-0088](docs/adr/0088-ota-artifact-hosting.md) Amendment 2). `POST /admin/releases?release=&arch=`
  takes the bare `pos-edge` executable as its body and minisign's signature line in `X-Pos-Minisig`,
  behind `console.ota.publish` and audited; `GET /admin/releases/{release}` lists the targets the
  cloud holds. Until this landed, only a test ever wrote the rows the store-facing artifact route
  reads.

  **Two mismatches were found building it, and both are corrected here.** The release workflow signed
  only the `.tar.gz`, while `UpdateInstaller::apply` installs a bare binary — so the trust chain's two
  halves named different files, and unpacking the tarball server-side would have installed bytes
  nobody signed. The workflow now stages and signs `pos-edge-<tag>-<target>.bin` per target, and that
  is the pair the upload carries. Separately, a release had three spellings — `v1.2.3` in the
  registry's docs, `1.2.3` in the binary (R1b strips the `v`) and `1.2.3` in a rollout's
  `target_version`. It now has one: the bare version, everywhere.

  **`PUT /admin/config/ota` refuses a `target_version` with no hosted artifact** (`422`, naming the
  field). Before the guard, promoting a typo succeeded and every store in the ring then fetched a
  `404` — which means "install nothing", so the fleet sat still with nothing in any log saying why.

  **Upgrade note** — no `PROTOCOL_VERSION` change and no migration: the routes are new, the registry
  table shipped with #177, and the `arch` field is additive. Two things change for an operator: the
  release runbook has an upload step (step 5) whose `release` value is the version **without** the
  tag's `v`, and a rollout can no longer name a version the cloud does not host. A deployment with no
  `[artifacts]` block cannot upload and therefore cannot promote — correct for a cloud that ships no
  edge releases, and unchanged for every other route.

- **The cloud can hold an edge release and hand it to a store** (roadmap `docs/roadmap-v3.md` **R2**,
  [ADR-0088](docs/adr/0088-ota-artifact-hosting.md)). `POST /sync/stores/{store_id}/artifact` returns
  the signed binary, with its detached signature in `X-Pos-Artifact-Signature` as lowercase hex so
  the body stays the raw artifact rather than a JSON envelope wrapping tens of megabytes.

  **It requires the `read_config` scope**, so no store needs re-provisioning — every provisioned box
  already carries it. And it is on `/sync`, not `/internal`: the proxy answers `404` to the whole
  `/internal` prefix from outside the box, so the path ADR-0054 originally pinned was unreachable by
  the only client it has (Amendment 1).

  **The cloud stays a dumb host.** It never signs and never verifies the minisign signature — the
  edge verifies against a trust anchor baked into its own binary, so a compromised cloud, a swapped
  blob or a spoofed host can make an update *fail*, never make a box install code. What the cloud
  does check is the registry's SHA-256, which catches a truncated upload or a corrupted blob before
  thirty megabytes travel.

  **Upgrade note** `bootstrap.sh` now creates the Garage layout, an artifact bucket and an S3 key on
  every deploy and writes them into `secrets/cloud.toml` as `[artifacts]`. Nothing to run by hand.
  A cloud with no `[artifacts]` block boots fine with the route off, which is the right posture for a
  deployment that ships no edge releases. No `PROTOCOL_VERSION` bump: `/sync` is unversioned and the
  request's `arch` field is additive.

  **Not yet an end-to-end update.** The edge's `OtaUpdater` is still not constructed and
  `cloud-sync-http` still posts to the old path; both are R5. This is the half that was missing, not
  the whole chain.

### Fixed
- **The gate register counted three chores as things only a person can do.** `docs/gate-register.md`
  shipped with "mint the Garage S3 access keys on the box", "set `RCLONE_REMOTE` on the box" and "add
  the certificate-export cron line" listed as human gates. None of them is.

  `bootstrap.sh` already runs on the box on every deploy — `.github/workflows/deploy.yml` invokes it
  over SSH — and already mints the database password, the broker token and the internal shared secret.
  Garage genuinely has to generate its own key pair, but `garage key create` is a command on the box
  and `bootstrap.sh` is a script on the box; the bucket-and-layout setup around it was deferred to
  backups and never written. `deploy.yml` already pipes four other values down the same SSH line, so
  `RCLONE_REMOTE` can ride along. And `bootstrap.sh` already calls `tls-export.sh` once, so it can
  write the crontab entry too.

  The three move to a new section — **manual today, and should not be** — which is a backlog, not a
  gate list. The page now states the test that would have caught them (*is there a reason a script
  cannot do this?*) before the tables rather than after, and records how the mistake was made: every
  row cited a document that describes the step as something an operator does, which is an accurate
  description of today and not evidence that a person is required.

  **What is left is genuinely human**: a secret a machine must not hold (the offline signing key), a
  one-shot value only a person can take custody of (the super-admin's TOTP), a judgement no config can
  make (which addresses to firewall), physical hardware, and outside bodies.

### Changed
- **The OTA artifact route moves off `/internal` before it is built** ([ADR-0088](docs/adr/0088-ota-artifact-hosting.md)
  Amendment 1, roadmap `docs/roadmap-v3.md` **R2**). [ADR-0054](docs/adr/0054-edge-cloud-http-client.md)
  pinned the edge's artifact fetch at `POST /internal/ota/artifact`, calling `/internal/` "the
  store-facing surface". It is not, and has not been since the proxy was taught to deny that whole
  prefix — `/internal/*` is the cloud's own trusted-network surface and answers **404 to every request
  from outside the box**.

  A store reaches its cloud through that proxy, so the pinned path is unreachable by the only client it
  has. The route is `POST /sync/stores/{store_id}/artifact` instead, on the store-facing family where
  the cloud resolves the tenant from the scoped key rather than trusting a body field.

  **It requires the `read_config` scope**, so no store needs re-provisioning: every box already carries
  it. The trade accepted is that `read_config` also authorises downloading a release artifact — broader
  than its name, and bounded by the fact that the scope exists to stop the VPS being an open download
  host, not to keep the binary secret. The artifact is signed and the edge verifies it against a
  build-baked anchor regardless.

  **No `PROTOCOL_VERSION` bump** — `/sync` is unversioned, the added `arch` field is additive, and the
  route has no existing server to break.

  **Upgrade note** For a fork that has implemented against ADR-0054's pinned path: it never worked from
  a store, and this is why. No code shipped at the old path, so nothing to migrate.

### Fixed
- **The shipped edge has no over-the-air update path, and its own source said otherwise.** Two
  docstrings in `crates/pos-edge/src/ota.rs` described the `UpdateInstaller` seam as the part "the
  shipped binary implements against the OS". No such implementation exists: the only one in the
  workspace is a test double, `OtaUpdater` is constructed only in `crates/pos-edge/tests/ota.rs`, and
  `crates/pos-edge/src/main.rs` does not mention OTA at all.

  This matters to a fork deciding whether it can ship updates to its fleet. The answer is **not
  yet**, and it is now written where a reader looks: the module says it is unreachable and names the
  two things that gate wiring it — the cloud does not serve `/internal/ota/artifact` (roadmap
  `docs/roadmap-v3.md` **R2**, [ADR-0088](docs/adr/0088-ota-artifact-hosting.md)), and that path is
  in any case proxy-denied to anything off the box, so R2 has to move it to the store-facing `/sync`
  surface first; and a real Linux installer is **R4**. Releases are still built and signed
  (see [`docs/release-runbook.md`](docs/release-runbook.md)); what is missing is the fleet delivering
  them to itself.

  **No behaviour changed** — there was none to change, which is the point.

### Added
- **Two documents for taking a fork into production** (roadmap `docs/roadmap-v3.md` A·P5, WS-F).
  [`docs/go-live.md`](docs/go-live.md) sequences the five runbooks that already existed — fork, cloud
  up, event bus, organisation, store box, publish, trade — and marks the twelve points where the
  sequence **stops for a human**. [`docs/gate-register.md`](docs/gate-register.md) is those gates as
  a standing reference: thirty of them, across human decision, per-store provisioning, privacy and
  legal, real hardware, and outside registration.

  Neither adds a requirement. Every row cites the file that already made the claim, because a
  register that invents obligations becomes a second source of truth and drifts from the first. What
  they add is **ordering and visibility**: each of these gates is silent — CI stays green, `main`
  stays green, the acceptance suite passes — and the failure surfaces at the first release, the first
  power cut, or the first audit.

  Also included: a pilot checklist, which is the subset that can only be answered on real hardware.

### Security
- **The release runbook told you to put the OTA trust anchor in the cloud's configuration tree.** Step
  3 said to record the public signing key "wherever the fleet's trust set is configured (the OTA trust
  configuration)". That is the tree the cloud publishes, and a key taken from there is a key an
  attacker who controls the cloud can choose — the verifier would check their artifact against their
  key. [ADR-0092](docs/adr/0092-artifact-trust-chain.md) settled this and
  [`fork-checklist.md`](docs/fork-checklist.md) was corrected at the time; the release runbook was
  not, so the two documents disagreed and the wrong one was the one an operator follows at the moment
  it matters.

  It now says what the code enforces: `POS_EDGE_TRUSTED_KEYS`, a repository **variable**, read at
  compile time. `pos-edge` exposes no runtime path for a trusted key at all — `trusted_keys()` takes
  no arguments and its parser is private — so an operator who followed the old step would have got a
  fleet that **cannot install an update**, refusing rather than trusting nothing, and would have hunted
  for it in configuration rather than in the build.

  **No code changed.** Nothing was ever built the wrong way; the instruction for doing so was.

### Added
- **A console link now carries the tenant it was read under** (roadmap `docs/cloud-admin-ux-plan.md`
  Track F1). Screens moved from `/people` to `/t/<tenant>/people`, and where a screen uses a store,
  it rides along as `?store=<id>`. Two consequences an operator will notice: a link pasted into a
  message opens on the same tenant for whoever receives it, and two browser tabs can sit on two
  different tenants at once — previously impossible, because the context lived in `localStorage`,
  which every tab of an origin shares.

  Old links still work. A bare `/people` redirects through the remembered tenant, so bookmarks and
  anything already sent survive.

  **The tenant is a path segment; the store is a query parameter.** That is not cosmetic. Every
  tenant-scoped screen needs a tenant — that is what the context gate already enforces — but the
  store is genuinely optional, and several screens (People, Reports, Campaigns) render perfectly
  well before one is chosen. Encoding an optional thing as a required path segment would have forced
  a placeholder like `/s/-/people` into the URL, which is the sort of thing this track has spent
  three slices removing.

  Console-level screens — the alert list, the audit trail, the admin roster, the account screens —
  keep bare paths, because they span every tenant and have none to carry.

### Changed
- **The console's screens are defined once instead of four times.** The router, the nav, the
  breadcrumb labels and the command palette were four hand-maintained lists that had to agree: 28
  `<Route>`s, the nav groups, a 28-entry label map, and the palette's own 11 hrefs. Nothing checked
  them against each other, and the dashboard has no test runner — so a screen added to three of the
  four, or a path typo'd in one, produced a dead link or a blank breadcrumb that only a human
  clicking through would find.

  They now all derive from one table. A screen's id is a union type, so a typo is a compile error;
  the router's component map is keyed by that union, so a screen without a component does not build.
  This was the prerequisite for moving 25 routes safely without tests to catch a broken link.

  **Upgrade note** No API, schema or permission change — this is the console's own URLs. Old
  bookmarks redirect rather than break.

- **Alerts can now be pushed off the console** (roadmap Track O2 slice 4, ADR-0073). The evaluator has
  been storing every firing alert since O2 slice 3, and the notification bell reads them — but nothing
  left the building. A store that went dark at 02:00 waited in the table until somebody opened the
  console. It now also posts newly-opened alerts to a webhook, so somebody can be woken.

  Set `alert_webhook_url` and `alert_webhook_secret` in the cloud's config file to arm it. Leaving them
  unset is a supported posture, not a gap: the in-console channel is the primary one and always runs.

  The batch goes as one signed JSON body — the same HMAC scheme and the same SSRF-vetted URL handling
  the tenant webhooks use (ADR-0032), because an alert batch is just another signed body to an
  endpoint an operator nominated. Twelve stores dropping off at once is one notification, not twelve.

  **A failed push never fails an evaluation pass.** The alerts are stored and in the console before a
  channel is asked, so an unreachable endpoint does not undo that, and the evaluator's health row
  stays green. The reason appears in that row's detail as `delivery_error` instead, so "alerts are
  firing and not arriving" is visible as its own state rather than looking like either half being
  down.

  The pushed body carries the alert kind, severity, tenant id, dedup key, the composed one-line
  summary and the numeric detail — and nothing else. There is no field a customer or employee
  identifier could arrive in, and a test pins the field set so a later change to what is sent to a
  third party has to be deliberate.

  **Upgrade note** No migration, no wire change, no permission change. Two new optional config keys.
  A URL without a secret, or a secret without a URL, is refused at boot rather than accepted: the
  first would deliver unsigned batches a receiver cannot tell from forgeries, and the second reads as
  armed while delivering nothing.

- **The employee roster can be read a page at a time** (roadmap v3 B3-4, ADR-0098). `GET
  /admin/employees?limit=&offset=` returns `{ items, total, limit, offset }`; naming no limit still
  returns the whole roster as an array, which is what the permission-node publish needs — a node
  compiled from a page would be missing whoever fell off it. This is the last of the five reads
  ADR-0098 identified.

  **This is T1 personal data either way, and paging does not change that** (ADR-0070). The page
  carries the same fields as the read beside it — the PIN hash is absent from both — behind the same
  `console.people.manage` gate. What changes is only how much of a roster crosses the wire at once:
  strictly less per response, never a new field, and nothing new reaching a log.

  The read's order gains a tiebreaker, `created_at DESC, id DESC`. That is not tidying: a staff CSV
  import writes a whole roster in one transaction and PostgreSQL's `now()` is transaction time, so
  those rows share one timestamp exactly. `created_at` alone does not order them, and a window over
  a non-total order can return the same person on two pages or on none.

  **The People console screen deliberately does not use it yet.** That screen reads the roster three
  times — the table, the assign picker, and the lookup that turns an assignment into a name — and
  only the first wants a page. Paging just the table would add a request rather than remove data;
  paging all three would make the picker offer only whoever is on the current page and show a raw id
  for anyone off it. The screen moves once it has a searching picker and a name it can resolve
  without the whole set.

  **Upgrade note** Migration `0045_employee_page_index.sql` adds `employees_by_tenant_newest
  (tenant_id, created_at DESC, id DESC)`; additive, `IF NOT EXISTS`, and the existing
  `employees_by_tenant` is kept. No wire change, no `PROTOCOL_VERSION` bump, no permission change.

### Fixed
- **The audit trail's "acting admin" filter can now be used** (roadmap Track G2 slice 4). It asked
  for an admin ULID while the table beside it showed an email, so an operator could see who made a
  change but had no way to filter by them — the id was never displayed anywhere to copy. It is a
  list of admins by name now, and the trail filters by whoever is picked.

### Changed
- **The People screen's employee table reads a page at a time.** A tenant with two hundred staff no
  longer waits for two hundred rows to see the first twelve.

  The search box and the column headers now ask the server, so searching finds people who are not on
  the current page, and sorting by name or staff code reorders the roster rather than the twelve rows
  on screen. Both were client-side before, which was correct while the screen held the whole roster
  and would have quietly become wrong the moment it stopped.

  This completes `#299`. The table was never the hard part: it was the last of three things on this
  screen that read the entire roster, and paging it while the other two still needed the set would
  have sent more data rather than less. The assignment rows and the assign picker had to move first.

- **The employee roster page can be ordered by name or staff code.** `GET /admin/employees` takes
  `?sort=newest|name|code` and `?order=asc|desc`; an unrecognised token is refused by name rather
  than quietly answered with the default order.

  This is groundwork rather than a visible change: the People screen's table sorts its headers
  client-side today, and once that table is paged, headers that reordered only the visible rows
  would be showing the operator something false. The orders have to exist on the server before the
  table can move.

  Both parameters need `?limit=`, like the search beside them. Every order ends in the employee's
  id, so a window over people who share a name cannot show one of them on two pages or on neither.

  **Upgrade note** Migration `0046_employee_name_index.sql` adds `employees_by_tenant_name
  (tenant_id, name, id)`; additive, `IF NOT EXISTS`. No index is added for the code order —
  `employees_code_key` from migration 0023 already covers it. No wire removal, no
  `PROTOCOL_VERSION` bump, no permission change.

- **The assign picker on the People screen searches the roster instead of holding it.** Typing part
  of a name or a staff code asks the server for the matches; `GET /admin/employees` takes a `?q=`
  now, which it did not before — the search the other four paged reads got in an earlier release
  never reached employees.

  An archived employee still appears in the results, listed and greyed out rather than filtered
  away. Hiding them would leave an operator searching for a real person, being shown nothing, and
  having no way to tell "no such person" from "that person is archived".

  This is the second of three steps toward paging that screen's employee table (roadmap `#299`).
  With the picker off the whole roster and assignment rows already naming their person, the table is
  the only reader left.

  `?q=` requires `?limit=`: a search without one is refused rather than answered with the whole
  roster, the same as everywhere else on this surface. The search matches the name and the staff
  code, never the PIN hash — which no read returns in any case.

- **An assignment now says who it is for.** `GET /admin/assignments` returned three ids and nothing
  else, so the People screen turned an assignment into a person's name by searching the whole loaded
  roster. The row carries `employee_name` and `employee_code` now, resolved by the server as it
  reads, and the screen uses them.

  This is the first of three steps toward paging that screen's employee table (roadmap `#299`,
  ADR-0098 B3-4). It could not be paged while three separate parts of the screen needed the whole
  roster; this removes one of them. The remaining blocker is the assign picker, which still offers
  every active employee from the loaded set and needs a server-side employee search first.

  Both new fields can be `null`. Nothing in the schema stops an assignment outliving the employee
  record it names, and a grant that still works is worth showing unlabelled rather than dropping
  from the list — the console shows the id in that case, exactly as it did before.

  **On personal data** The name and code are T1 (ADR-0070), so this read returns personal data where
  it returned only ids. It is a redistribution, not an expansion: the same caller, behind the same
  `console.people.manage` permission that already lets them read the roster, already saw the name on
  this screen — by fetching the entire roster to find it. Nobody new can see anything new, and the
  PIN hash is no more selected here than anywhere else.

### Fixed
- **A page past the end of the employee roster no longer reports a headcount of zero.** The window
  count rides on the returned rows, so an empty window — a page past the end, or a pager sitting on
  page four of a roster that has since shrunk — had no row to read it from and answered `0`. That
  reads as "this tenant has no staff", and the console's pager sizes itself from that number. The
  count is now taken properly when the window comes back empty.
- **The same fix now covers every paged read**, not just the roster: vouchers, the media library,
  the audit trail and the item master all reported a total of zero for an empty page, and all four
  now report the size of the set the page is past the end of. For the two reads that filter — the
  audit trail's seven optional filters, and the item search that also looks inside per-locale names
  — the count still counts what the filter matched, so a search with no hits still reports zero,
  which was always the true answer there. One helper (`store::window_total`) now owns this for all
  five reads, so a sixth inherits it rather than having to remember it.

### Added
- **The console surface has a generated API document** (roadmap v3 B5, ADR-0019 amended).
  `GET /admin/openapi.json` serves it and `docs/openapi-admin.json` commits it — a second document
  beside `/v1`'s, because the two have different audiences: `/v1` is what an integrator calls with a
  scoped API key, `/admin` is what a console calls with a session cookie. A fork writing its own
  console, a mobile admin app or an internal tool now has the routes written down.

  Each documented route carries its path, method, parameters, every status code it can answer, and the
  AIP-193 error envelope those failures carry. Success bodies are described in prose rather than as
  schemas — the amendment explains why in full, but briefly: an `/admin` handler returns a `pos-proto`
  wire type directly, and generating schemas from those means putting a doc-generation crate inside the
  backbone, which is its own decision.

  **Coverage is 7 of 137 routes, and the other 130 are listed in the code.** Not as a comment: four
  tests hold that list honest, and the one that matters is that a route added without either an
  annotation or a list entry **fails the build**. The surface can no longer grow undocumented without
  someone writing it down. The list only shrinks.

  **Upgrade note** No migration, no new dependency, no wire change. `/v1/openapi.json` is untouched.

### Changed
- **The nine `/admin/inventory` handlers that act on one record now read one record.** Reading,
  editing or deleting a single ingredient, recipe or supplier used to list the tenant's whole
  collection of that kind and scan it in the cloud — so the work grew with the tenant's catalogue on
  every one of those calls, and a chain with three thousand ingredients carried all three thousand
  out of the database to answer "show me this one".

  `InventoryStore` gains `get_ingredient`, `get_recipe` and `get_supplier` beside its `list_*`
  methods, following the shape `CampaignStore::get_campaign` already set. The `store-postgres` impl
  answers them from `inventory_items`' primary key, `(tenant_id, kind, entity_id)`, so **no
  migration** — the index the read needs has been there since migration 0037.

  No behaviour changes: the same record, the same `ETag`, the same `404` when it is absent. That is
  precisely why the route tests could not tell the two apart, and why the fake now records which
  reads a handler made — the test asserts that a single-record route never calls `list_*`, which is
  the only way a regression to scanning is visible at all.

### Added
- **The audit trail can be read from either end** (ADR-0098 slice B3-3). The paged form of
  `GET /admin/audit` takes `?order=newest|oldest`; absent, it is `newest`, which is what the read has
  always returned. `oldest` is what an incident reads like — a `since_ms`/`until_ms` window in the
  order the actions actually happened, rather than backwards from the end of it.

  The order is named for the trail rather than spelled `asc`/`desc` like the item read. There the
  direction is relative to a named `?sort=` field; this route has none, so `asc` would have to mean
  "ascending in time" — one word doing a different job on two routes, which is worse than two
  spellings. A token that names neither order is refused with the two that do.

  Only the *paged* read takes it. On the windowed read `?limit=` already means "the most recent this
  many", so `?order=oldest&limit=200` has two honest readings — the newest two hundred shown
  earliest-first, or the earliest two hundred — and they are different sets. The route refuses the
  parameter there rather than picking one silently.

  The Audit screen's "when" header is now a control that re-reads the trail; its other three columns
  are no longer sortable. Each of those already has an *exact filter* above the table, which answers
  "what did this admin do" better than ordering a million-row trail by a low-cardinality column, and
  until now their headers sorted the twenty-five rows on screen as if they were the whole trail.

  No `?sort=` on this route, and no `?q=`: free-text across a trail of millions cannot use a btree,
  so it is a trigram-index decision — a new dependency, and an ADR of its own. Deliberately not made
  here.

  **Upgrade note** No migration. `audit_log_by_tenant_newest` (migration 0042) is
  `(tenant_id, at DESC, id DESC)`, and the oldest-first order is that index read backwards — verified
  against real PostgreSQL, which serves it with no sort step above the scan.

- **The item master's page can be searched and ordered by the server** (#159, ADR-0098 slice B3-3).
  `GET /admin/catalog/items` takes `?q=`, `?sort=` and `?order=` alongside its page bounds. `q` is a
  case-insensitive substring of the item's name *or any of its per-locale names* — ADR-0074 exists
  because operators type Vietnamese, and a search that read only the primary name would miss the
  case those translations are for. `sort` is one of `newest` (the default and the order the unpaged
  read has always used), `name`, or `status`; anything else is refused by name rather than answered
  with the default.

  The Items sub-screen now pages server-side at 25 with its own search box and sortable headers. Its
  previous box filtered the twelve rows on screen; this one asks the server, so it searches the whole
  master. The tax-class and category columns are no longer sortable: the value shown in each is a
  label resolved from another table, and sorting the page by it would order twenty-five rows as if
  they were the master.

  **Upgrade note** One additive index migration, applied idempotently on boot (ADR-0017):
  `0044_catalog_item_name_index.sql`, which carries the name order so `?sort=name` stops a scan
  rather than sorting every item a chain sells. `?sort=status` has no index by design — the column
  holds one of two values.

- **Three more `/admin` lists answer a second, paged question** (#158, ADR-0098 slice B3-2). Media,
  the audit trail and the item master each keep the read they had and gain a page beside it, along
  with the index the page needs.

  - `GET /admin/media` — `?limit=` returns a page plus the library's size; absent, the whole library
    as before, which is what the item image picker needs. The Media screen now pages its grid at 24.
    The saving is not response size: the grid mounts an `<img>` per asset and each fetches a
    thumbnail, so an unpaged grid of a large library is hundreds of requests when the screen opens.
  - `GET /admin/audit` — here the trigger is `?offset=`, not `?limit=`. `limit` already meant "the
    most recent this many" (ADR-0069), defaulted to 200 and clamped at 500, and the console sends it
    today; changing what it returns would break a request already in flight. `limit` alone therefore
    answers exactly as before. The Audit screen pages at 25 server-side.
  - `GET /admin/catalog/items` — `?limit=` returns a page plus the size of the item master. Absent,
    the whole master, permanently: the menu compiler and five of the six console consumers of this
    read fill pickers, not tables, and a menu compiled from a page would be missing whatever fell
    off it. The Items screen still reads the whole master; it cannot move to a page until `?q=`
    exists to replace its search box (B3-3).

  New `Pager` component in the CRUD kit, extracted from `DataTable`: the media library is a grid, not
  a table, and a second hand-rolled pager beside the first is how two pagers come to disagree about
  whether the last page is reachable.

  **Upgrade note** Three additive index migrations, applied idempotently on boot (ADR-0017):
  `0041_media_page_index.sql`, `0042_audit_page_index.sql`, `0043_catalog_item_page_index.sql`. The
  media and item reads also gain a primary-key tiebreaker in their `ORDER BY` (ADR-0098 decision 9),
  which changes the order *within* a batch of rows sharing a `created_at` — those rows previously had
  no defined order at all, which is the bug the tiebreaker fixes.

### Fixed
- **A server-paged table sorted the page it was showing, not the set** (#159). `DataTable`'s own
  docstring promised no local re-sort in server mode, but `visible()` returned the sorted rows in
  both modes and a header click was gated only on the column having a client-side accessor. The
  Audit screen went server-paged in #158 with four sortable columns, so clicking one there ordered
  the twenty-five rows on screen as if they were the whole trail.

  Server mode now sorts only when a column names a `sortField` *and* the caller passes `onSort`,
  and the click asks the caller to re-read the set; without both, headers render as plain text
  rather than as controls that quietly lie. The Audit screen's headers are inert until its own
  `?sort=` lands.

- **Creating or renaming a catalog item failed against real PostgreSQL** (#158). Both statements bind
  the item's per-locale names as text into a `jsonb` column and cast it `$4::jsonb`. PostgreSQL infers
  a bare `$N::jsonb` parameter *as* `jsonb`, and `tokio-postgres` then refuses to send a Rust `&str`
  for it — so the statement failed at the driver and the route answered `503`. Every other `jsonb`
  parameter in the tree already carried the `$N::text::jsonb` double cast; these two, added with the
  per-locale names column (migration 0029), did not.

  It shipped because nothing in the store-postgres suite had ever called `insert_item`: the console
  tests run against the fake, which has no parameter types to get wrong, and the integration file
  only exercised placements. A round-trip test now covers both writes, and it fails if the cast is
  reverted.

- **`?limit=abc` on `/admin/audit` returned axum's raw query rejection** rather than an AIP-193 body
  naming the field (#158). One parser now owns the parameter on that route, so an unparseable value
  is refused the same way an out-of-range one is.

- **An empty optional id means "unset" on every field, not two of them** (#280). Seven typed
  wrappers sat on `parse_optional_ulid`, and two — `brand_id` on a store, `parent_menu_id` on a menu
  — matched on `Some(text)` without trimming or filtering. So `""` meant "malformed, `400`" for
  those two fields and "unset" for the other five optional-ULID fields on the same surface, with
  nothing in the types to notice and nothing in the API description to warn a caller. Clearing a
  select box sends `""`; the console only avoided the refusal because every call site happened to map
  it to `null` first, so one screen forgetting that would have produced a `400` an operator could not
  act on.

  The seven wrappers are gone. Every caller passes its own constructor (`BrandId::new`,
  `MenuId::new`, …) to the one helper, which is now the only place the rule is written: absent, empty
  or whitespace is `None`; anything else must be a ULID. Three unit tests pin the rule across id
  types, and a route test exercises the two fields that disagreed — because a unit test cannot see a
  handler that stops calling the helper.

### Changed
- **The six keyed upserts on `/admin` are now a create and an update, and both are conditional**
  (#287, ADR-0095). Campaigns, ingredients, recipes, suppliers, menu placements and layout buttons
  each had one `upsert_*` behind them. An upsert answers the same way whether it inserted or
  replaced, so "add this" and "replace this" were the same request — and for the four of these whose
  key is supplied by the caller, adding something at a key another operator had just used silently
  discarded their work. A recipe for an item that already had one lost its bill of materials; adding
  an item to a menu repriced the one already on it, and because the per-channel prices are the
  price-change journal (ADR-0069) that overwrite was recorded as a set with no `before` to compare
  against.

  Each seam is now `create_*` — which inserts and answers `409 ALREADY_EXISTS` when the key is taken
  — plus `update_*`, which applies only at the version the caller read. In `store-postgres` the
  create is `ON CONFLICT … DO NOTHING … RETURNING xmin::text`, so "the key was taken" is one round
  trip with no window between a check and the write, and the update is the existing `AND xmin::text
  = $n` predicate. The `409` matters beyond tidiness: these seams share one error channel that
  collapses everything to `503 the service is unavailable`, which tells a caller the server is broken
  and invites the retry that can never succeed — the trap #152 fixed on `set_station`.

  Three `POST` routes are new, on the collection URIs that had none:
  `POST /admin/inventory/recipes`, `POST /admin/catalog/menus/{menu_id}/placements` and
  `POST /admin/catalog/layout-buttons`. Each names its own key in the body, because the collection
  URI has no room for it and the matching `PUT` keeps taking it from the path.

  **Upgrade note.** The six `PUT`/`PATCH` update routes now require `If-Match`; a write without one
  is refused with `400` and `{"field": "if-match", "reason": "REQUIRED"}`. Their reads answer with
  the version to send back: a single-resource read stamps an `ETag` header, and every `list_*` row
  now carries an `etag` field — additive, so a reader that ignores it sees exactly the fields it saw
  before. That last part is what makes the update reachable at all: a console edits from the list,
  and one header cannot describe many rows. The console is updated in the same change; a
  fork's own client needs the same two moves (send `If-Match` on an update, `POST` to the collection
  to create).

### Added

- **ADR-0098's paged cohort is five lists, not six, and its criterion says what it meant** (#157).
  Documentation only; no behaviour changes.

  Two corrections found by going to build B3-2 and reading what the routes actually do.

  **`devices` is out.** The record justified paging it with "fleet size", but
  `GET /admin/stores/{store_id}/devices` lists **one store's** devices — a few terminals, a KDS, a
  printer or two. The count is bounded by what someone installs in one shop and fleet size never
  enters it. It is authoring-bounded like `areas` and `tables`, so it joins the thirty-four lists that
  keep exactly what they have.

  **The criterion is reworded.** Its second half read "the row count is driven by data volume or fleet
  size rather than by a human authoring each row" — a cleaner-sounding line that is wrong twice. It
  reads as a binary when what matters is magnitude: a chain's `items` and a company's `employees` are
  each authored by a human *and* number in the thousands, so the wording excluded two lists it was
  written to include. It now reads "the row count can plausibly reach a size one response should not
  carry", which is the question that was always being asked.

  **And a third correction, which is a correctness one.** Every `created_at` in this schema is
  `DEFAULT now()`, and PostgreSQL's `now()` is *transaction* time — so every row written by one
  transaction carries the identical timestamp. Measured: six `media_assets` rows inserted in one
  transaction gave **one** distinct `created_at`. A list ordered `created_at DESC` alone therefore has
  no order at all across each batch it contains, and `LIMIT`/`OFFSET` over a non-total order can
  return a row on two pages or on neither. There is a live batch path — the CSV import rail loads a
  whole item file in one transaction.

  So the record now carries it as a rule (decision 9): a paged read needs a total order — the
  timestamp plus the row's own key — and an index covering all of it. `media_assets`,
  `catalog_items` and `employees` each need both halves; `audit_log`, already ordered
  `at DESC, id DESC`, needs only its index widened.

  Nothing shipped on any of the old wording: `vouchers` (#156) is in the cohort under either reading
  of the criterion, and already ordered `created_at DESC, voucher_id DESC` — a total order it
  inherited from the query that was already there rather than from design.


- **A campaign's voucher codes can be read a page at a time** (#156). ADR-0098 slice B3-1: the paging
  vocabulary, and `vouchers` — the acute case — proven through it end to end.

  `MAX_VOUCHER_BATCH` is 10 000 codes per mint and batches accumulate against a campaign, so a
  promotion with three drops holds 30 000 rows the Campaigns screen fetched in one go.
  `GET /admin/campaigns/{id}/vouchers?limit=25` now answers `{items, total, limit, offset}` instead.

  The console's own saving is larger than a page. Measured while wiring it, the screen never rendered
  the codes at all — it rendered `existingVouchers().length`, one number — so it was pulling up to
  30 000 rows across the wire to count them in the browser. It now asks for `?limit=1` and reads
  `total`: the same number, one row. (ADR-0098's own measurement said the screen "fetches and
  renders" every code; the second half was wrong and is corrected in that record.)

  **Without a `limit` the route is unchanged**, and permanently so. An operator distributing a
  promotion needs every code to print or mail, and "page four of the flyer run" is not something they
  can ask for; more generally, a default limit is what would put a page between the menu compiler and
  the items it compiles. So `PageRequest` cannot be built without a caller naming a limit, and at the
  seam `list_by_campaign_page` sits beside an untouched `list_by_campaign`.

  A `limit` or `offset` that is not an integer in range is a `400` naming the field, never a silent
  clamp — and an `offset` sent **without** a `limit` is refused too, because honouring it silently
  would answer a question nobody asked and dropping it silently would lose one somebody did.

  **Migration `0040_voucher_page_index`** rides along, and is the reason the page is cheap rather than
  merely small: `vouchers_by_campaign` is `(tenant_id, campaign_id)` while the read orders by
  `created_at DESC, voucher_id DESC`, so PostgreSQL was fetching every matching row and sorting the
  lot. The new index carries the sort, so the index walk *is* the ordering and `LIMIT` stops the scan
  instead of truncating a finished one. `total` comes from `COUNT(*) OVER()` in the same statement as
  the rows — one round trip, one snapshot, so the count cannot disagree with the page it labels.


- **ADR-0098 — paging is a second read, not a change to the read that exists** (#155).
  Documentation only; no behaviour changes.

  F2's plan booked one line for this: `?limit=&offset=&q=&sort=&order=` on the `admin_list_*`
  handlers, `{items, total}` in reply, threaded through the store seams. Measured, that shape would
  have broken working screens. `GET /admin/items` has six console consumers and only **one** is a
  table — the other five read the whole set to fill an item picker or resolve an id to a name — and
  inside `http.rs` another 29 list-call sites across 22 functions are not a list route at all: the
  menu compiler makes six of them, the publish and export paths more, and nine are `GET`/`PATCH`/
  `DELETE` handlers on one ingredient, recipe or supplier that list the whole table to find their one
  row. Give `limit` a default and a menu compiles without the items on page two, with nothing raised
  anywhere.

  **So the unpaged read stays, permanently.** `?limit=` absent returns today's array of every row;
  `?limit=` present returns `{items, total, limit, offset}`. Two supported forms, not a migration
  with a deprecated side: a caller asking for every item so it can offer them in a dropdown is a
  correct caller, not a laggard. At the seam the existing `list_*` is untouched and a second
  `list_*_page` is added beside it, so the fifteen whole-set callers churn not at all —
  the shape `AuditStore` already has, with `query(filter)` beside `list_recent(limit)`.

  **Six lists get paging, thirty-three do not, and four never will.** The criterion is both halves:
  a screen renders the whole set into the DOM *and* the row count grows on data rather than on
  someone typing. That selects `vouchers` (10 000 codes per mint, batches accumulating — three drops
  is 30 000 rows in one response the Campaigns screen renders), `media`, `audit`, `items`,
  `employees`, `devices`. Menus, sections, placements, tax classes, ingredients, suppliers, stores
  and the rest are bounded by authoring effort; `permissions`, `capabilities`, `countries` and
  `locales` are compile-time registries where a pager is pure ceremony.

  Four smaller decisions, each with a reason the plan did not have: `?q=` works **without** `limit`,
  because a dropdown over 3 000 items wants type-ahead and not page four; `sort` is a per-route
  whitelist refused by name when unknown, because an arbitrary identifier reaching `ORDER BY` is an
  injection surface no parameter binding covers *and* an unindexed sort is the problem paging was
  meant to solve; a `limit` or `offset` out of range is a `400`, never a silent clamp, since clamping
  lets a client believe it read a tail it never reached; and `total` comes from `COUNT(*) OVER()` in
  the same statement as the rows, because an estimate makes the pager lie about a set an admin is
  actively editing.

  `DataTable` gains optional server-paging props and, when given them, stops sorting and slicing
  locally — handed 25 of 812 rows it currently renders "1–25 of 25", which is worse than no pager at
  all.

  One correction to this work's own earlier record rides along. An earlier pass called F2 item **B1**
  (composite `(tenant_id, created_at)` indexes) a non-task because 37 of 51 tables already carry a
  tenant-scoped index. That measured the wrong property — tenant scoping is not sort coverage — and on
  the six lists this pages, four have an index that finds their rows but cannot serve their `ORDER BY`
  (`vouchers` is `(tenant_id, campaign_id)` but orders by `created_at DESC, voucher_id DESC`;
  `catalog_items`, `employees` and `devices` order by `created_at DESC` off a bare tenant index). Left
  alone, `LIMIT` would shrink the response while the database still sorted all 30 000 rows on every
  page. So B1 is real, narrower than the plan had it, and a prerequisite rather than a separate item:
  each paging slice carries the additive index its own query needs.

  Delivery is three slices: the vocabulary plus `vouchers` proven end to end, then the remaining five
  lists, then `q`/`sort`/`order`.

- **ADR-0097 gives the `/internal` routes a key of their own** (#148). Documentation only; no
  behaviour changes yet.

  #144 closed the internet-facing half of the `/internal` exposure in both deploy lanes. What stayed
  open was everything *inside* the trust boundary — another container on the compose network, a pod
  reached by service IP, a fork terminating TLS where the deny does not reach. This record decides a
  shared secret on the three routes: `X-Pos-Internal-Key`, a `404` refusal with no oracle, the secret
  in the cloud's existing mode-0600 `deploy/secrets/cloud.toml`, and a **boot refusal** when it is
  absent rather than "absent means off" — because `CloudConfig` is not `deny_unknown_fields`, so one
  transposed letter in the key name would otherwise leave a file that looks armed in front of an open
  surface.

  **The measurement that shaped it: nothing calls these routes yet.** `/internal/ingest` has no HTTP
  caller at all (the production feed is the in-process NATS cursor); `/internal/reconcile` has no
  non-test caller; `/internal/ota/report` has one client whose only exercise is the contract suite.
  So the usual reason this change is expensive — a deployed fleet that cannot be assumed upgraded —
  does not apply, and a three-mode compatibility window would be machinery bought for nobody. That
  stops being true when R5's artifact route or a real `report()` caller lands. The cost is a step
  function and we are still on the cheap side of it.

  Two things the record is careful to say rather than imply. The secret is **cloud-side only** and
  must not reach a store box: `cloud-sync-http` serves `/activate` and `/internal/*` through one
  transport built on every box with a `cloud_url`, so an unconditional header would ship a
  fleet-wide secret to an unauthenticated pre-activation endpoint on every unprovisioned machine —
  worse than today. And a shared secret **cannot make `/internal/ota/report` attributable**: that
  route reads `tenant_id`/`store_id` out of the body, so any key-holder could still file a report for
  any store. The fix for that is the surface, not the secret — it moves to
  `/sync/stores/{store_id}/…` when it gains a real caller.

  Amends ADR-0040 §53, ADR-0087 §40 and ADR-0078 §32, each of which recorded the no-authentication
  posture as deliberate. Each was right about the network and silent about what happens inside it.

- **ADR-0095 splits what ADR-0094 left into three shapes** (#141). Documentation only; no behaviour
  changes.

  ADR-0094 described its remaining scope in one line — "the `PUT` routes that replace a record in
  place … plus the config tree's whole-document save" — which reads as one homogeneous group.
  Measured on the seams it is three, needing three different answers, and this record decides two of
  them and names the third as the owner's call.

  **The config tree writes conditionally on the `ConfigVersionId` it already has.** There are twelve
  `ConfigTreeStore::save` call sites, every one reconstructing the tree from a freshly loaded state
  and replacing the whole four-layer document plus its history — so two concurrent publishes lose one
  of them entirely. But `ConfigTree::current_version()` already exists, is already carried to the
  edge, already recorded in the audit trail and already listed by the history routes the Config
  screen renders. No new token, no new column, and — unlike every other entity here — a `412` that
  can name a version the operator recognises.

  **Tax rates and the translation grid get a version on the collection, keyed by tenant.** These are
  the only two genuine whole-collection replaces. Neither handler reads before it writes: the
  read-modify-write is the *console's*, which is the same lost update with an operator's thinking
  time in the middle rather than a request's. This shape brings the first migration in this line of
  work — ADR-0094's "no schema change" property holds for records and does not extend to collections.

  **Six keyed upserts are deferred, deliberately.** `upsert_ingredient`, `upsert_recipe`,
  `upsert_supplier`, `upsert_campaign`, `set_placement` and `set_layout_button` are create-or-replace
  at a key, not collection replaces as an earlier scoping note had them. `If-Match` alone cannot
  separate a create from a blind overwrite, so this needs either a second conditional header
  (`If-None-Match: *`) or a split of each seam into an explicit create and update. The record
  recommends the split and leaves the choice to the owner, because it changes six route shapes.

  Two corrections ride along: ADR-0094's exclusion of `If-None-Match` as "bandwidth, not correctness"
  is withdrawn for keyed upserts, where it is exactly a correctness question; and this work's own
  earlier plan of one collection version for all eight remaining seams is recorded as wrong on the
  measurement, fitting two of them.

### Security
- **The three `/internal` routes now require a key of their own** (#149). ADR-0097, merged as #148.

  `/internal/ingest`, `/internal/reconcile` and `/internal/ota/report` authenticated nothing. #144
  closed the internet-facing half in both deploy lanes; what stayed open was everything *inside* the
  trust boundary — another container on the compose network, a pod reached by service IP, a fork
  terminating TLS where the deny does not reach. Reaching them meant forged events for any tenant, an
  existence oracle over a store's event log, or a falsified fleet report.

  They now require `X-Pos-Internal-Key`, compared with the same `constant_time_eq` `/admin/setup`
  uses. The refusal is a **`404` with one wording** for absent, wrong and not-configured alike: a
  `403` confirms the route is there, which is what both proxy denies and ADR-0050's activation
  refusal decline to confirm. **This does not replace either proxy deny** — the key decides who
  inside the network may call them, the deny decides whether the internet reaches them at all — and
  the `tls-modes` gate still fails if either deny is removed. Its hint text, which taught the
  opposite, is corrected here along with the four `http.rs` doc comments, both proxy configs,
  `k8s/README.md` and the adapter's.

  The guard sits on `/internal/reconcile`'s **handler**, not its router: that router also carries
  `/admin/reconcile`, which is behind a console permission and must not start demanding an operator's
  secret. A test pins that.

  **Fail closed at boot.** `CloudConfig::validate` refuses to start without the secret, rather than
  treating absence as "authentication off". That is the load-bearing part: `CloudConfig` is not
  `#[serde(deny_unknown_fields)]`, so `internal_shared_secet` — one transposed letter — deserialises
  to `None`, and under "absent means off" that typo leaves a mode-0600 file that reads as armed in
  front of an open surface. The value lives in `deploy/secrets/cloud.toml` beside the Postgres
  password and `admin_setup_token`, wrapped in a newtype whose `Debug` is redacted, and
  `bootstrap.sh` mints it.

  Two things ADR-0097 is explicit about rather than leaving implied. The secret is **cloud-side
  only**: `cloud-sync-http` serves `/activate` and `/internal/*` through one transport built on every
  box with a `cloud_url`, so an edge attaching the header unconditionally would send a fleet-wide
  secret to an unauthenticated pre-activation endpoint on every unprovisioned machine. And a shared
  secret **cannot make `/internal/ota/report` attributable** — it reads `tenant_id`/`store_id` out of
  the body, so any key-holder can still file a report for any store. That one is fixed by the
  surface, not the secret: it moves to `/sync/stores/{store_id}/…` when it gains a real caller.

  **Upgrade note.** An existing deployment will not start until its `cloud.toml` gains
  `internal_shared_secret`; the refusal names the field and how to generate one. `bootstrap.sh` mints
  it for new boxes, and `docs/fork-checklist.md` lists it as required. Nothing in the fleet is
  affected: the routes have no production callers, which is what made now the cheap time to do this.

- **A refused webhook registration reported what the cloud's own DNS resolver saw** (#147).

  `POST /admin/webhooks` vets a URL against SSRF before storing it (ADR-0032), and the refusal was
  `format!("the webhook URL was rejected: {rejection}")` — the `Display` of `SsrfRejection`, rendered
  straight into the response body. Four of that enum's six variants describe the caller's own string
  and are harmless. The other two are decided by **this server's resolver**:

  - `ForbiddenAddress(IpAddr, ForbiddenReason)` interpolates the address the host resolved to and its
    class, so `url=https://internal-thing.corp.example/` answered
    `the webhook host resolves to a forbidden address (10.4.12.7): private network`;
  - `Unresolved` answered `the webhook host did not resolve`.

  Between those two and a success, a caller could tell three outcomes apart — resolves publicly,
  resolves privately *to this address*, does not resolve — which turns the register route into an
  internal name-to-address mapper, one name per request. The request was correctly never made, and
  then the refusal handed over exactly the reconnaissance the block exists to deny. The route is
  behind `ConsolePermission::ManageWebhooks`, which `Ops` holds as well as `Owner` and `Admin`, so a
  day-to-day console operator is not obviously the right audience for the cloud's network topology.

  **The fix.** `SsrfRejection::caller_message` draws the line the type did not have: the four
  caller-string variants keep their exact wording, and the two resolver-decided ones collapse into
  one sentence — *"the webhook URL must point at a public address"* — that says what is required
  without saying which way it failed. `Display` is unchanged, and the full rejection now goes to
  `tracing::warn` instead, the same "coarse to the caller, full to the log" shape the delivery path
  in `webhook/runner.rs` already used. The submitted URL is deliberately **not** logged: a webhook
  URL routinely carries its own bearer token in the path or query, and this is a log.

  Collapsing rather than merely dropping the address is the point — keeping the two messages
  distinct would still separate "exists in internal DNS" from "does not resolve", which is most of
  the oracle. Three tests pin it, all mutation-checked: `Display` keeps the address while
  `caller_message` drops both it and the class; `Unresolved` and `ForbiddenAddress` are byte-equal to
  a caller; and the HTTP-level refusal body carries neither the address nor the class.

  **What this costs.** An operator registering a webhook no longer sees which address the name
  resolved to; it is in the server log. Nothing else changes — the vetting policy, the status code
  and the four precise messages are identical, and no stored endpoint is affected.

- **The Kubernetes lane published the `/internal` routes to the internet** (#144). `pos_cloud` serves
  three routes under `/internal/*` that authenticate **nothing**, by design — they are documented as
  private-network-only: `/internal/ingest` (the reconciliation re-push), `/internal/reconcile` (the
  id-diff endpoint) and `/internal/ota/report` (the fleet report). The Compose lane has closed them at
  the proxy since the exposure was first found, and an xtask gate fails if that deny is ever removed.

  The optional Kubernetes lane had no equivalent. Its `Ingress` routed `/` as a single prefix, so a
  fork that followed `k8s/README.md` published all three, unauthenticated — event injection, an
  id-holding oracle, and falsifiable fleet state for any tenant. Nothing caught it because nothing
  looked at both lanes at once.

  The skeleton now denies `/internal/` with 404 (not 403: a 403 confirms the route exists), and the
  `tls-modes` gate checks **both** lanes, so they cannot drift apart again. An allow-list of public
  prefixes would have been fail-closed and needed no annotation, but it is not available here — the
  console is a single-page app served from the `/` catch-all, so every client-routed path must reach
  the backend.

  **Upgrade note.** If you run the Kubernetes lane, apply the updated `k8s/pos-cloud.yaml` and then
  **verify** the deny took effect — `curl -s -o /dev/null -w '%{http_code}\n' https://YOUR_DOMAIN/internal/reconcile`
  must print `404`. ingress-nginx ignores snippet annotations when the controller runs with
  `allow-snippet-annotations: false` (its default since 1.9), and it does so silently, which looks
  exactly like success. If the curl prints anything else, implement the deny with a front proxy or a
  controller-native rule before running the lane in production. Whether these routes should *also*
  carry an application-layer secret remains open.

### Fixed
- **A stale-version station update answered `503` instead of `412`, and `main`'s integration job
  has been red for twelve consecutive runs** (#152). Two defects, one of them user-facing, both from
  Q3c and both invisible to the pull-request gate.

  The last green `main` was #139. The next merge, #140, is the one that introduced the bad query
  below, and `main` has failed on every push since — twelve runs, eleven merges shipped on top of a
  red base without it being visible to anyone reading a pull request.

  `store-postgres`'s `set_station` updates `kitchen_stations`, but the follow-up probe that decides
  *why* the update matched nothing — a stale version, or no such station — queried **`floor_stations`,
  a table that does not exist**. So the conflict path raised a database error: every stale-version
  station update answered `503 the cloud database failed` rather than the `412` the whole slice was
  built to return, and a caller was told the server had broken when their own request was simply out
  of date. Shipped in #140.

  Separately, #143 added `catalog_tax_rate_versions` beside `catalog_tax_rates` but not to the test
  suite's `TRUNCATE` lists, so a version row survived into the next case and a tenant that had never
  saved rates reported one.

  **Why the pull-request gate could not see either.** These tests live behind `store-postgres`'s
  `integration` feature and run only in the post-merge `main` workflow, against a real PostgreSQL.
  The gate neither compiles nor runs them, by design — so both defects merged green and `main` went
  red and stayed red, with nothing in front of a reviewer to say so.

  The `TRUNCATE` lists are now the **complete** table set rather than a hand-maintained subset. That
  is the durable half: the suite passed only against a freshly created database, and a second
  consecutive run failed nine cases, which is precisely the leakage that hid the tax-rate defect. It
  now runs twice in a row green, so a new table is caught by the suite rather than by whoever reads
  the `main` badge next.

  A `RowUpdate::NotFound` assertion is added for `set_station`, the branch of that probe that had
  none anywhere — which is how a probe against a non-existent table went on looking correct.

### Changed
- **The last thirteen plain-text `400`s join the envelope, and two refusals that knew which field
  was wrong stop throwing it away** (#151). The second half of the migration ADR-0096 deferred; with
  it, every refusal the cloud emits is on the AIP-193 envelope.

  These thirteen were held back from #150 because they need their **producer** changed, not just
  their response. Four request-to-record builders (`build_campaign`, `build_ingredient`,
  `build_recipe`, `build_supplier`) returned a bare `&'static str`, and their doc comments said why:
  a `Result<_, Response>` trips `clippy::result_large_err`. That constraint is real and is respected
  — the new `FieldRefusal` is three `&'static str`s, 48 bytes, well under the 128-byte threshold —
  while fixing what the bare string cost, which is that a refusal naming no field is one no console
  can mark.

  **Two of them discarded what they knew.** `build_campaign` checked both schedule bounds in a single
  `if` that OR'd them and named neither, so a caller who sent both minutes could not tell which was
  refused. `RollupWindow::new` did the same in a loop over `[from, to]`: it knew which bound failed
  and threw that away one line before the caller needed it. Both are split, and `RollupWindow::new`
  now returns a `WindowError` enum rather than a message — the domain module knows *what* is wrong,
  and only the route knows the wire names, so the field naming stays at the route.

  Nested values carry their **full path**: `action.rate`, `action.amount`,
  `conditions.schedule.start_minute`, `conditions.schedule.end_minute`, `conditions.channels`. A
  console marking `details.field` can address the input a reader actually sees, rather than being
  handed a leaf name it must guess the parent of.

  `parse_known_tokens` now answers `enum_refusal` — the same shape the 27 sites of Q3b slice 4a use —
  so an unrecognised sales channel or payment method names the field (`enabled`, `accepted`) and
  lists what is accepted, where before it said `"{token} is not a recognised value"`: neither the
  field nor the set. `UNSPECIFIED` is deliberately excluded from that list, and tested: it exists so
  an older client can read a newer server's value, is refused by the same parser, and listing it
  would invite a caller to send a token that earns this refusal again.

  **One refusal deliberately names no field.** `from` after `to` is a refusal in which neither bound
  is wrong on its own; attributing it to `from` would send half of callers to the input they typed
  correctly. The message names both, and `details` stays empty — the standing rule being to name
  only the field that actually was at fault.

  **Upgrade note.** A client reading these thirteen refusals was reading `text/plain`; it now reads
  the same envelope every other refusal in the cloud uses, and the console has parsed both shapes
  since Q3a. No protocol-version bump: nothing is renamed or removed.

- **Nine of the cloud's last plain-text `400`s join the envelope, and five that were already on it
  named a field the caller never sent** (#150). The first half of the migration ADR-0096 deferred.

  ADR-0096 left 22 plain-text `400` sites as "a migration backlog, not a missing capability" —
  expressible with `InvalidArgument` today, so nothing blocked them. This takes the nine whose
  handler already knows which field was wrong; the remaining thirteen need their producer changed
  (four builders, `RollupWindow`, `parse_known_tokens`) and are the next slice.

  An unknown permission id, capability flag or API-key scope now answers
  `("permissions"|"flags"|"scopes", "INVALID_ENUM_VALUE")` — `INVALID_ENUM_VALUE` rather than
  `UNKNOWN_REFERENCE` because these are compile-time catalogues, and the repo already reserves
  `UNKNOWN_REFERENCE` for a reference to a stored row.

  **The correction worth reading.** Five `details` entries shipped by earlier Q3b slices named
  fields that do not exist on the wire: `("tax_rates", …)` on four refusals whose request struct
  carries `rates`, and `("vendor_policies", …)` on one that carries `policies`. A console that marks
  the field named in `details` would have marked a field the caller never sent — worse than no
  detail at all, because it is confidently wrong. Both are corrected here. They were found by
  reading the request structs rather than the neighbouring code, which is the only way this class of
  error surfaces.

  Two sites get **no** `details`, deliberately. The CSV import parser addresses a row and byte offset
  in an opaque uploaded body, and the request's only named input is a `tenant_id` query that is not
  what failed. The QR order converter returns a bare `&'static str` shared with `POST /v1/orders` and
  the relay, so naming a field would mean changing a converter with three callers; that site was
  simply the last of the three still answering raw text.

  The webhook SSRF refusal gains `("url", …)` with a new reason, `FORBIDDEN_DESTINATION`, shared by
  the two variants #147 collapsed into one message. That sharing is the point and is tested: giving
  them distinct reasons would tell a caller apart what the message just stopped telling them, and
  reusing `INVALID_FORMAT` would send them to fix a URL whose syntax is fine.

- **Nine refusals that could not say what was wrong with them now can** (#146). ADR-0096.

  `ErrorStatus` mapped to 400/401/403/404/409/412/429/500/503 and nothing else, so a handler that
  wanted to answer *"every field is valid and the document they make together is not"* had no status
  to say it with. Nine invented a body of their own instead, and the console grew a parser per shape
  until it ran out: a translation grid rejected for a missing `en` fallback answered
  `{"missing_fallback":["menu.rice"]}`, which nothing in the console read, so the operator's toast
  rendered that JSON verbatim in a dialog where a sentence belongs.

  **`ErrorStatus::Unprocessable` (`UNPROCESSABLE`, `422`) is the twelfth status**, added exactly as
  ADR-0094 added the eleventh. Older readers are safe by construction: `Open<ErrorStatus>` keeps an
  unrecognised token as text rather than failing the parse, so a build that predates this shows an
  unknown status and still renders the message.

  **Seven of the nine become it**, and how each carries its reasons follows from what it knows.
  Violations that arrive from `pos-core` as prose — a routing rule naming a station that does not
  exist, capability flags contradicting each other, a price book that will not compile — join into
  `message`; inventing a `field` for them would fabricate structure the domain never produced. The
  translation grid *does* know its fields, so each offending key becomes one
  `{"field": "<key>.en", "reason": "REQUIRED"}` entry, `.en` because it is the `en` value that is
  missing and a path beats a name a reader has to interpret.

  **Two of the nine were never 422**, which is the correction this slice found while reading them:
  `/admin/setup` and `/admin/invites/accept` refuse a password shorter than the minimum, and that is
  one field out of range. They become `400 INVALID_ARGUMENT` with `("password", "OUT_OF_RANGE")`,
  matching the `("pin", "OUT_OF_RANGE")` shape already used everywhere else for exactly this, and
  the message now says what it accepts instead of only that the value was rejected.

  The test that separates the two groups is the one that justifies the status at all: **is there a
  field to name?** If yes it is `400`, whatever the handler answered before. That is also why the
  cheap alternative — call all nine `INVALID_ARGUMENT` and add nothing — was rejected: it is one line
  of code and a permanently worse error message, sending an operator hunting for a typo in a form
  where there is none.

  The console loses a parser rather than gaining one: four body shapes become two — the envelope,
  and raw text for what Q3 has not converted yet. `pos-edge`'s two `ErrorStatus` matches gain the
  arm they are exhaustive to force, and the relay's status-to-class mapper drops its `_` arm so the
  next status added cannot be swallowed into `failed_precondition` by default.

  **Upgrade note.** No `PROTOCOL_VERSION` change: none of the nine routes appears in
  `docs/openapi.json`, and the envelope's shape is unchanged — only a new value in an existing field
  the type was built to tolerate. A client that branches on `response.status` sees `400` instead of
  `422` on the two password routes.

- **Tax rates and the translation grid stop losing concurrent saves** (#143). Q3c slice 5b, ADR-0095
  shape C. Both screens load a whole grid, an operator edits one cell, and the screen `PUT`s the
  whole grid back — a read-modify-write with a human thinking in the middle, so two operators editing
  different cells kept only the second, and the first operator's edit was gone with nothing to notice
  it by. The read now carries the version it was taken at as an `ETag` and the save carries it back
  as `If-Match`; a save against a version the collection no longer holds is `412 VERSION_MISMATCH`,
  and the console reloads and says what happened. `If-Match: *` asserts "nothing saved here yet" and
  is refused once a version exists.

  The two collections version themselves differently, and the difference is about the *write*, not
  the entity. The translation grid is one `jsonb` row per tenant, so a save updates it in place and
  its own `xmin` versions it — **no migration**. The tax-rate table is many rows and a save deletes
  and reinserts all of them, destroying every `xmin`, so it gets a per-tenant version row (migration
  `0039_tax_rate_versions`) claimed inside the same transaction as the replace. ADR-0095 had booked
  both as needing a version table; measuring the schema showed only one does.

  The CSV translation import (`POST /admin/translations/import/apply`) is the exception on the third
  write: it **merges** its rows into whatever grid is stored rather than replacing it, so losing a
  race is a stale read, not a lost update. It retries — reload, re-apply its own keys, save again —
  three attempts before a `412`, the same shape as the config node publishes in #142.

  **Upgrade note.** Two console routes change contract: `PUT /admin/catalog/tax-rates` and `PUT
  /admin/translations` without an `If-Match` header are now refused with `400 INVALID_ARGUMENT`. One
  additive migration (`0039_tax_rate_versions`), applied idempotently on boot, greenfield with no
  backfill. The CSV import routes are unchanged on the wire.
- **The config tree stops losing concurrent publishes** (#142). Q3c slice 5a, ADR-0095 shape A. Every
  one of the twelve `ConfigTreeStore::save` call sites used to reconstruct the whole four-layer
  document from a freshly loaded state and write it back unconditionally, so two publishes that
  overlapped kept only the second — the first operator's layer edit *and* its history entry were
  gone, silently. `load` now returns the version it read at and `save` applies only if the stored
  state is still at that version; under `store-postgres` that is the same `xmin` compare-and-swap the
  record-shaped entities use, with no migration.

  Two kinds of write sit on that precondition, and they behave differently on purpose. A config layer
  is a map of nodes, so the ten routes that publish *nodes* — floor, stations, inventory, campaigns,
  tax, locale, channels, tender, QR, permissions, capabilities — write only their own keys and
  **retry** when they lose a race: reload, re-apply your keys to the winner's layer, save again. A
  menu publish and a floor publish that overlap now both survive, where before one was lost and where
  a plain refusal would have shown someone a conflict about a key they never touched.

  The two routes where an operator types the document itself — `PUT
  /admin/stores/{store_id}/config/{level}` and `POST /admin/stores/{store_id}/config/rollback` —
  **require `If-Match`** and answer a stale one with `412 VERSION_MISMATCH`, because there a second
  writer really does destroy typed work. `If-Match: *` asserts "this store has never been published
  to"; it is the one place in the console a wildcard is accepted, and it is refused once a version
  exists. The Config screen sends the version it last read, reloads on a `412` and says what
  happened. The background activator for scheduled publishes takes the same precondition: losing the
  race leaves the row pending for the next pass rather than failing it.

  **Upgrade note.** Two console routes change contract: a `PUT /admin/stores/{store_id}/config/{level}`
  or `POST /admin/stores/{store_id}/config/rollback` without an `If-Match` header is now refused with
  `400 INVALID_ARGUMENT`. No other config route changes, and no migration is needed.
- **The floor and people entities join the conditional-write mechanism** (#140). Q3c slice 4: areas,
  tables, kitchen stations, employees and role templates — the five remaining record-shaped entities
  in the console — go through the same `Version`/`Versioned`/`UpdateOutcome` seam and the same
  `xmin::text` compare-and-swap as slices 2 and 3. Five `PATCH` routes now require `If-Match` and
  answer a stale one with `412 VERSION_MISMATCH`.

  **These five keep their response shapes; only the header is new.** Unlike the registry and the
  catalog, their `PATCH` answers `204` and their `POST` answers `{"id": …}` — and their update
  payloads do not carry every field (an `AreaUpdate` has no `store_id`), so there is no
  representation the handler could return without inventing one. The version goes out in the `ETag`
  header, which is where RFC 9110 puts it for exactly this response, and the five read-one routes
  (`GET /admin/floor/areas/{id}` and its four siblings) now carry one too. A caller that wants the
  record re-reads it; a caller that wants to keep writing already holds the token.

  **Setting a PIN is deliberately not version-gated.** `PUT /admin/employees/{id}/pin` writes one
  field no other console form edits, so there is no edit for it to clobber. It does move the row's
  version, because it is a write — the seam's doc says so, and the in-memory fake models it, so a
  fake never hides a conflict the real store would report.

  **The console follows in the same change**: the five `update*` client methods take the version the
  row was read at, over a new `requestVoidIfMatch`; the Floor, Stations and People screens reload on
  a `412` rather than offering a retry, with `floor.stale`, `stations.stale` and `people.stale` in
  `en`/`vi`.

  One tidy-up rides along: `registry_outcome` and `catalog_outcome` were byte-identical, so the five
  new seams share a single `update_outcome` instead of adding three more copies that could drift.

- **The catalog's nine authoring entities join the conditional-write mechanism** (#139). Q3c slice 3:
  items, tax classes, item categories and sub-categories, display categories and sub-categories,
  modifier groups, menus and menu sections — nine `create`/`list`/`update` triples, eleven `PATCH`
  routes, through the same `Version`/`Versioned`/`UpdateOutcome` seam and the same
  `xmin::text` compare-and-swap slice 2 built. Nothing new was invented; the point of the slice is
  that the mechanism carried across without special cases.

  **`CatalogStore` splits into two shapes, and only one of them is this mechanism's problem.** The
  nine record-shaped entities have the registry's shape and get the registry's treatment. The two
  keyed upserts — `set_placement`/`remove_placement` and `set_layout_button`/`remove_layout_button` —
  do **not**: a caller there asserts a fact at a key rather than replacing a record it read, and on a
  first write there is no prior version to name, so `If-Match` does not fit without also introducing
  `If-None-Match`. They are deferred to the slice that takes the config tree's whole-document save,
  where the question is the same one — how a *set* replaced wholesale avoids losing a concurrent edit
  to a different member — and the seam's doc comment says so rather than leaving the omission to be
  discovered.

  **Reads that are not writes strip the version.** The menu compiler, the layout compiler and the
  CSV export take the authoring records themselves: a version is a writer's concern, and carrying one
  into a compiled book or an export column would be noise. Each of those three load sites says so at
  the point it discards it.

  **The console follows in the same change**, since the routes now refuse a write without `If-Match`:
  the nine catalog `update*` client methods take the version the row was read at, and the Catalog and
  Layout screens reload on a `412` rather than offering a retry, via a shared `isStale` and one new
  `catalog.stale` string in `en`/`vi`.

### Changed
- **The console's four registry writes stop losing edits: `ETag` on read, `If-Match` on write, `412`
  on a stale one** (#138). Q3c slice 2, the mechanism [ADR-0094](docs/adr/0094-console-optimistic-concurrency.md)
  frames, proven end to end on the smallest complete family — tenants, brands, stores, devices.

  **`pos-cloud` gains `version::{Version, Versioned, UpdateOutcome}`.** A `Version` is an opaque
  string: not a counter, not a timestamp, not ordered, and the type offers no way to ask. A
  `Versioned<T>` serialises as the record's own fields plus an additive `etag` — `#[serde(flatten)]`,
  so nothing moves or is renamed — which is how a **list** carries a version per row where a header
  cannot. `UpdateOutcome` replaces the seam's `bool` with three answers, because a conflict and an
  absence need different things from the caller and a bool cannot tell them apart.

  **Beneath it, `xmin`.** `UPDATE … WHERE <key> AND xmin::text = $n RETURNING xmin::text` — one
  statement, so the compare and the swap cannot be separated, and no migration. Zero rows is
  ambiguous, so a probe on the failure path separates `NotFound` from `VersionMismatch`. The
  comparison is on **`xmin::text`, not `$n::xid`**: casting caller-supplied text to `xid` raises
  `invalid input syntax for type xid` on anything non-numeric, which would turn a client's stale or
  garbled tag into a `500` instead of the `412` it earned. A live-Postgres case covers exactly that,
  alongside the two that no fake can prove — that `xmin` moves on every `UPDATE`, and that a replayed
  version changes nothing.

  **On the wire.** A read-one and a write answer `ETag: "<token>"`; every list row carries the same
  byte-identical token as `etag`; a write requires `If-Match`. Two headers are refused on purpose and
  differently: `W/"…"` is `INVALID_FORMAT`, because RFC 9110 §13.1.1 compares `If-Match` *strongly*
  and a weak validator can never match — accepting one would accept a tag that can only ever fail;
  and `*` is `WILDCARD_NOT_ACCEPTED`, because it is legal HTTP for precisely the last-write-wins
  behaviour this removes. An absent header is an ordinary `400` naming `if-match` as `REQUIRED`,
  checked **after** field validation so a caller with a bad ULID still hears about the ULID.

  **A create now answers with the version it starts at**, in the body and an `ETag` on the `201`.
  Without it an admin who has just made a record would have to re-list the collection before editing
  it — a round trip to learn something the server already knew.

  **Breaking, deliberately, on `/admin`:** the four registry `PATCH` routes now refuse a request with
  no `If-Match`. Accepting an absent header as "no opinion" would have left the silent clobber in
  place *as the default*. The console is updated in the same change: `ApiError.isStale` names the
  `412`, and the Stores screen reloads and says so rather than offering a retry — a retry would
  re-apply the overwrite through a different door. `/admin` has one consumer, this repository's
  console.

### Added
- **ADR-0094 — the console stops losing edits** (#137)
  ([ADR-0094](docs/adr/0094-console-optimistic-concurrency.md)). No behaviour change in this entry:
  the record lands first because it adds a status to the `pos-proto` error envelope and changes the
  shape of every mutating `/admin` seam, and roadmap v3 **Q3c** books it that way.

  **Every master-data edit in the console is last-write-wins, silently.** Two managers open the same
  item; one saves a price, the other saves a name from a form loaded thirty seconds earlier. The
  `PATCH` body carries every field, so the second save writes the stale price back over the first.
  Both see success, and the audit trail records two updates with no hint that one erased the other.
  Measured: **24 mutating `/admin` routes** (15 `PATCH`, 9 `PUT`) with no concurrency control, and
  **nothing anywhere in the tree** — cloud, console or edge — that sends or reads `ETag`/`If-Match`.
  The worst case is the config tree, whose `save` replaces a store's whole four-layer document, so
  two admins publishing *different nodes* concurrently lose a whole node rather than a field.

  The decision: a `Version(String)` **opaque token minted by the adapter** and never interpreted
  above it, with Postgres `xmin` beneath — one atomic
  `UPDATE … WHERE id = $1 AND xmin = $2 RETURNING xmin`, no extra round trip on the happy path, and
  **no migration**, since the column is already on every row including ones a fork adds later. Zero
  rows means the write did not apply; one probe then separates `NotFound` from `VersionMismatch`, so
  a `404` and a `412` stay distinguishable. The Postgres tie lives in one adapter, which is the
  direct mitigation for the fork cost the basis carries.

  On the wire: a **strong** `ETag: "<token>"` on read-one, the same byte-identical token as an
  additive `etag` field on each row of a list (a header cannot carry per-row versions), `If-Match`
  **required** on write, and `412` on mismatch. An absent `If-Match` is an ordinary `400` with
  `details: [{"field": "if-match", "reason": "REQUIRED"}]` rather than a tenth status.

  **`ErrorStatus` gains one variant, `VERSION_MISMATCH` → `412`** — the envelope maps to
  400/401/403/404/409/429/500/503 and could not express `412` at all, the same gap the Q3b sweep hit
  with `422`. It is deliberately *not* named `PRECONDITION_FAILED`: beside the existing
  `FAILED_PRECONDITION` (409) that is two tokens differing only in word order with different codes.
  Safe by construction — `status` is `Open<ErrorStatus>`, so an older client reads it as unrecognised
  and still gets an intact `code`, `message` and `details`.

  **Two corrections recorded rather than glossed.** (1) The scoping pass that framed the choice
  reported `updated_at` as unmaintained on the catalog tables, "21 of 35" `UPDATE`s not setting it.
  Re-measured: **38 `UPDATE`s, 23 setting it, 15 not** — and all 15 are state transitions (revoke,
  resolve, accept, mask, cancel), not master-data edits. `updated_at` is still rejected, but for the
  correct reason: it is a *convention* nothing enforces, so it fails **open** — a future write that
  forgets it would let a stale `If-Match` match and the clobber proceed with the client believing it
  was protected. (2) The previewed token was `W/"xmin-1847302"`; the shipped form is `"1847302"`.
  RFC 9110 compares `If-Match` strongly, under which a weak validator never matches, and naming the
  mechanism in the token would put "this is Postgres" on the wire — the very cost the opaque seam
  exists to contain.

  Those same 15 transition writes are also what the decision is modelled on: they already
  compare-and-swap in the `WHERE` clause (`AND revoked = false`, `AND resolved_at IS NULL`,
  `AND status = 'PENDING'`). The tree knows the technique; what it lacked was a predicate for the
  case where the expected prior state is the whole record. That case now has one.


### Changed
- **The last 48 refusals answer in the AIP-193 envelope, and Q3b is closed** (#136). Slice 4c.

  These are the per-handler tail: no repeated family, so this one really is a conversion rather
  than a helper. A field detail wherever a field is at fault, reusing the tokens the earlier slices
  established (`REQUIRED`, `OUT_OF_RANGE`, `INVALID_ENUM_VALUE`) before adding `INVALID_FORMAT`,
  `UNKNOWN_REFERENCE`, `DUPLICATE`, `WRONG_KIND`, `MUTUALLY_EXCLUSIVE` and `ALREADY_EXISTS`.

  **Two deliberate absences.** No `details` on any credential or authorisation refusal — naming the
  missing permission tells a caller probing its own reach exactly which grant to go after — and none
  on a server fault, where there is nothing the caller did wrong and nothing it can change.

  **One deliberate exception.** `/admin/assignments` wants exactly one of `store_id` or
  `employee_id`, and its refusal names **both**. Every other multi-field refusal in Q3b was narrowed
  to name only what was wrong; here the *pair* is the mistake and neither field alone is at fault.

  Seven of the 48 were outside `http.rs` — the sign-in and session rejections, the API-key scope
  denial, the embedded-dashboard fallback and the QR-disabled page — and only a sweep across the
  whole crate found them. `StatusCode` is now an unused import in three of those files.

### Known limitation
- ~~**Three refusals cannot join the envelope: it has no `422`.**~~ **Closed by #146 (ADR-0096),
  and it undercounted twice.** There were **nine**, not three: this note saw only four bare-text
  sites and listed three of them, missing the menu compiler's, and it did not mention the five that
  answered JSON bodies of their own invention at all — four `{"violations":[…]}` and one
  `{"missing_fallback":[…]}`. The second omission mattered more than the first, because the
  `missing_fallback` body had no reader in the console, so it was a live defect rather than an
  untidiness, and a note that never named it could not surface it.

  Its judgement about the two password routes was also wrong in the safe direction: it called `422`
  "the honest code for all three", and one field out of range is a `400`. Recorded rather than
  quietly amended, because the reason it happened is worth keeping — the note was written from a
  grep for the status code, and a grep for a code finds the sites that answer it, not the sites that
  should.

- **An absent entity and a down dependency each answer in one shape** — 103 sites (#135). Q3b
  slice 4b, and the last of the cloud's repeated refusal families.

  `"no such X"` was written out sixty-one times in thirty-five spellings, and `"the X service is
  unavailable"` forty-two times in sixteen. Unlike the ULID and closed-set refusals nothing here was
  *wrong* — only repeated — so `not_found(entity)` and `service_unavailable(service)` are about the
  next absence and the next outage arriving in the same shape rather than about fixing a defect.

  Neither carries a `details` array, deliberately: `details` names a field the caller got wrong, and
  a caller asking after a store that does not exist got its fields right. Naming one would send a
  client to fix an input that was fine.

  `service_unavailable` is `ErrorStatus::Unavailable`, not `InvalidArgument`, which is the
  distinction deriving the HTTP code from `ErrorStatus::http_code` exists to keep: an outage is
  **retryable** and the request was fine. A client that reads a `503` as its own fault stops
  retrying something that would have succeeded.

  Two refusals keep their own wording rather than joining the shape: `"the store has no published
  configuration"` (six routes — the store and its config tree both exist; what is missing is a
  published *version*), and `/admin/setup`'s `"setup is not enabled"`, whose vagueness is the point,
  since `"no such X"` would imply a setup resource that might exist under another name.

- **A refusal about a closed set now lists the set the parser actually accepts** — 22 sites, and the
  set has one home per field instead of one per sentence (#134). Q3b slice 4.

  `"status must be active or archived"` was written out at **eighteen** routes. The sentence and the
  parser were two separate statements of the same set, which is the shape that goes wrong quietly: a
  third status would have needed eighteen edits and got none of them, leaving eighteen routes naming
  a set that no longer existed. The same held for `role` (2 routes), the admin `status`, and `level`.

  Each set now lives on its own enum — `EntityStatus::ALL` and `AdminStatus::ALL` are new,
  `AdminRole::ALL` and `ConfigLevel::ORDER` already existed — and **both** the wire parser and the
  refusal's prose are derived from it. A new variant updates all of them. `details` carries
  `(field, "INVALID_ENUM_VALUE")`, which is what a console needs to mark the offending `<select>`.

  The prose is unchanged for three of the four fields, including all eighteen `status` routes, so
  only the `details` array is new. `level` reads `"level must be tenant, brand, store, or device"`
  where it read `"level must be one of tenant, brand, store, device"` — the one wording that moved
  onto the shared shape.

- **A refusal about a value out of range names the field, and only the field that was wrong.** The
  five range checks — the staff PIN's length, `cutoff_hour`, a store's opening hours, a voucher
  batch's `count`, a scheduled publish's `effective_at_ms` — now carry an `OUT_OF_RANGE` detail.
  `"open_hour and close_hour must be in 0..=23"` named **both** hours whichever one was out of
  range, the same over-naming the ULID refusals had in slice 3; it names the one that was.

- **Every ULID a caller sends is now parsed in one place, and a refusal names the field that was
  actually wrong** — 177 refusal sites, half of everything the cloud can refuse (#133). Q3b slice 3.

  A `?tenant_id=` or a `{store_id}` that is not a ULID was refused by hand at every route. Written
  177 times it drifted three ways at once: **63 different phrasings** of the same sentence; **no
  `details` array**, so the console had nothing to mark the offending input with; and — the one that
  actually misinforms a caller — about **120 sites whose message named every field the handler had
  looked at** rather than the one that failed. `PATCH /admin/stores/{store_id}` answered "the store
  id or tenant_id is not a ULID" whichever of the two was broken, so an operator whose tenant id was
  fine was sent to check it.

  `parse_ulid_fields` now owns all of them. It takes the fields, so it can name exactly the ones
  that failed — which is what makes the ambiguous shape *unwriteable* rather than merely fixed here:
  no signature in the module accepts a message about fields it did not check. One bad field still
  reads `"tenant_id is not a ULID"`, the wording 51 of these sites already used, so the common case's
  prose is unchanged and only its `details` array is new. Two bad fields read `"store_id and
  tenant_id are not ULIDs"` and carry one `NOT_A_ULID` detail each.

  Three of these were already on the envelope from slices 1 and 2 and were **wrong**: the order
  relay's ack, its pull, and `/internal/reconcile` each named *both* of their id fields as
  `NOT_A_ULID` when only one could be. Those are the shipped instances of the defect this slice
  removes, and they are fixed here.

- **The store-facing surface answers in the AIP-193 envelope too** — `/sync`, `/internal`,
  `/activate`, the order relay and the QR guest pages (#132). Q3b slice 2, after slice 1 did `/v1`.

  Every refusal on these routes now carries `{"error":{"code","status","message"}}`, with `details`
  naming the field at fault. Where a route can refuse for more than one field — `/sync/stores/{id}/config`
  can object to the path's store id *or* the query's `held_version` — the answer now says **which**.

- **A second copy of the status→HTTP-code map is gone.** `relay.rs`'s `relay_error` re-implemented
  the map `ErrorStatus::http_code` owns, exactly as `orders.rs`'s `intake_error` did before slice 1
  deleted that one. Finding it **twice** is what makes it a pattern rather than an oversight — the
  shape was being copied from one route module to the next — so the map has one home again.

- **A refusal about two fields now names only the ones actually missing.** `propose_device` refuses
  when either `name` or `address` is blank, and told every caller to check both. A store that sent a
  name and forgot the address is now told about the address alone. The condition is unchanged; only
  the answer got specific.

  The four QR guest rejections are converted together, in the one `match` that maps them, so the
  guest page has a single shape to render whatever went wrong. None of them carries `details`: an
  unrecognised table code deliberately says nothing field-level, because that is the one arm where
  naming what was wrong would help someone guessing at codes.

  As in slice 1, **these bodies had no test coverage** — the pos-cloud suite passed unchanged
  through the conversion. Three new tests cover the multi-field cases, the per-field improvement is
  mutation-verified, and `/admin` (the bulk of what remains) is the next slice. The edge's own
  routes stay deliberately out of scope: their only consumer is the embedded UI on the store LAN.

### Changed
- **Every `/v1` refusal now comes back in the documented AIP-193 envelope** rather than as plain
  text (#131). Q3b slice 1, over the helper Q3a built.

  `/v1` is the surface with callers outside this repository, so the shape of a refusal there is a
  contract rather than a detail — and it was the one surface still answering with a bare string. The
  bodies of `POST /v1/orders`, `GET /v1/orders`, `GET /v1/stores/{id}/rollups/daily` and the shared
  bearer gate are now `{"error":{"code","status","message"}}`, with `details` naming the field and a
  stable reason where a field is at fault.

  Per the fork owner's standing decision, `/v1` bodies may change shape freely because nothing
  consumes them yet, and **`pos-api-version` is deliberately not pulled forward** by this change.

- **A duplicated status→HTTP-code map is gone.** `orders.rs`'s `intake_error` re-implemented the
  map that `ErrorStatus::http_code` owns — the very thing `api_error`'s contract exists to prevent
  ("the HTTP status is not a parameter: it is derived from `status` … which is where the mapping is
  stated once"). Both copies agreed on every arm, which I verified, so this was a drift hazard
  rather than a live defect; but the place it would have surfaced is the surface with third-party
  callers, where a code disagreeing with its body is a client reading a different error from the one
  the server meant.

  `reason` in `details` is a stable `SCREAMING_SNAKE` token and `message` is prose for a person.
  They are separate on purpose: the message can be reworded or translated freely, while the reason
  is what a client branches on — wording a client depends on is wording that can no longer be
  improved.

  **These bodies had no test coverage at all**, which is how they stayed plain while the envelope
  existed beside them. They have it now, including that the `401` keeps its `WWW-Authenticate`
  header (set after the body is built, so replacing the body construction is exactly where it would
  have been lost) and that a credential failure carries **no** field-level detail, which would be
  the oracle the one generic `401` exists to avoid.

  `/sync`, `/internal`, the QR guest surface and `/admin` still answer in plain text; they are later
  slices. The edge's own routes are deliberately out of scope — their only consumer is the embedded
  UI on the store LAN, which already parses plain text, so there is no third party on the other end.

### Added
- **A counter screen, so a cashier can find a takeaway order and charge it** (#130). The second half
  of [ADR-0093](docs/adr/0093-bill-keyed-on-order.md): #129 made a tableless order chargeable
  through the API, and this is what an operator can actually touch.

  A relayed marketplace order, a public-API order or a QR counter order sits on **no table**
  (ADR-0064), so it appears on no floor plan and there is nothing to tap. The routes #129 added were
  reachable only by someone who already knew the order's ULID — which is not something an operator
  can be handed. **`GET /api/orders/open`** is the counter's equivalent of `GET /api/floor`: every
  counter order still owing money, each with the daily queue number staff shouted, what is on it,
  what it owes, and the bill already open on it if there is one.

  The new **Counter** screen (`ui/`, nav between Floor and Kitchen) lists them and charges one on the
  same cash/card pad the table pay screen uses, so a cashier's hands learn one pad. An order's ULID
  is never shown.

  Three details that are decisions rather than defaults:

  - **The queue number is read, never allocated.** `QueueNumberAuthority` gains
    `queue_number_for(store, order)`. Re-asking `allocate_queue_number` would have answered too
    (it is idempotent by order) and would have *minted* a number for any order a screen refresh
    happened to mention — including floor orders, which must never have one.
  - **That read carries no business date**, because `queue_allocations` is keyed by
    `(store, order)`. A date-keyed read would lose the number of exactly the order most likely to
    still be sitting on the counter: one left unpaid past the day's cutoff.
  - **One authority, shared.** `compose` now wraps it in an `Arc` and hands the same value to the
    intake path and to the counter route. Two authorities over one store would be two counters, and
    the list would show no number for an order that plainly has one. `Arc<Q>` implements the trait
    by delegation, which is what makes sharing possible at all — the trait returns `impl Future`, so
    it is not dyn-compatible.

  An order whose bill is already open **stays on the list** and is marked as resumable, because the
  cashier still has to take the money; the screen settles that bill rather than asking for a second
  one, which the edge refuses. A settled order leaves the list. Both are covered end to end in the
  Q1 acceptance suite, over the routes a device calls.

### Fixed
- **A takeaway order can be paid for.** Implements
  [ADR-0093](docs/adr/0093-bill-keyed-on-order.md) (#129).

  A relayed or counter order was accepted, priced from the store's own menu, stored, queued and
  fired — and then could not be charged for. `Edge::open_bill` was the only path to a bill anywhere
  in the tree and it took a `TableId`, so a tableless order (ADR-0064) had no table to pass, to be
  occupied, or to resolve from. Not degraded: impossible.

  `open_bill_for_order(actor, order_id)` is now the primitive and `open_bill(actor, table_id)`
  delegates to it, so the floor keeps the `Occupied` gate ADR-0072 owns rather than moving it into a
  caller. `BillRecord.table_id` is an `Option`, and settling cycles a table only when there is one.
  Two routes make it reachable from a till: **`POST /api/orders/{id}/bill`** and
  **`GET /api/orders/{id}/check`**, the counter's equivalents of the table-keyed pair.

  A floor order billed through the order-keyed route still makes its table's
  `Occupied → AwaitingPayment` move, because the domain decides that from the order rather than from
  which route was called. A counter order needs no `tables_enabled` capability to be charged — the
  capability gates table service, not taking money, so a takeaway-only store no longer has to turn
  table service on to use its till.

  Two defects were found and fixed while implementing, both invisible until a bill could be
  tableless:

  - **A counter bill did not survive a restart.** The projection's `billing.bill.opened` fold arm
    recorded a bill only when it could find a table for the order, so a rebuild dropped a takeaway
    bill entirely — a box restarted between opening one and settling it came back not knowing the
    bill existed. Covered by `a_counter_bill_survives_the_restart_that_used_to_drop_it`.
  - **One-bill-per-order was being enforced by accident.** A table can only be `Occupied` once, so
    the table state machine refused a second bill on its way through and nothing had to say so.
    Keying on the order removes that accident, so the projection now carries an explicit order→bill
    index; without it, two open bills on one order would mean two receipt numbers and a guest
    charged twice for one meal.

  The Q1 acceptance suite now settles a relayed takeaway order end to end over the routes a device
  calls, where before it asserted the gap and recorded why.

### Changed
- **Requesting a second bill on a table that already has one now says so.** It was reported as an
  illegal table transition (`AwaitingPayment` cannot be asked for a bill); it is now
  `a bill is already open on that order`. Still `409 Conflict`, so no client changes — the message
  is just the true one (#129).
- **`table_state` is omitted from a bill response for an order with no table** rather than always
  present (#129). The edge UI's typed client already declared the field optional and guarded on its
  absence, so nothing in `ui/` changed; a fork reading it as required should treat it as optional.

### Added
- **ADR-0093 — a bill belongs to an order, not to a table**
  ([ADR-0093](docs/adr/0093-bill-keyed-on-order.md)). No behaviour change in this entry: the record
  lands first because it changes an app-layer domain contract, and roadmap v3 finding #9 booked it
  that way.

  **Takeaway revenue is uncollectable at the counter** — not degraded, impossible. `EdgeOrderIn`
  accepts a relayed order, reprices it, stores it transactionally and issues a queue number, and then
  the path ends: `Edge::open_bill` is the only route to a bill anywhere in the tree, and it takes a
  `TableId`, gates on that table being `Occupied`, resolves the order *through* the table, and stores
  a non-optional `table_id`. A takeaway order is tableless by design (ADR-0064). So every takeaway
  order a store accepts is priced, queued, fired — and cannot be charged for.

  **What the ADR found, which shrinks the follow-up:** the event log has never been table-keyed.
  `BillingBillOpened` carries only `{bill_id, order_id}`, and not one billing event mentions a table.
  The projection already admits it — `table_for_order` exists with the comment *"a bill event names
  its order, not its table"*. So the durable truth already says a bill belongs to an order, and the
  coupling is entirely the edge's in-memory reach for it. **No `pos-proto` change, no
  `PROTOCOL_VERSION` bump, and no migration.**

  Decided: `open_bill_for_order(actor, order_id)` becomes the primitive with `open_bill(actor,
  table_id)` delegating (the floor keeps its `Occupied` gate, which ADR-0072 owns);
  `BillRecord.table_id` becomes `Option<TableId>` and settling cycles a table only when there is one;
  and the projection gains an order→bill index that refuses a second bill on one order — because the
  table state machine was enforcing that as a *side effect*, and moving to an order key would
  silently drop it. Two open bills on one order means two receipts and double payment for one meal.

  Rejected, and it was the tempting one: minting a synthetic `TableId` per takeaway order needs no
  domain change at all, and puts rows that are not tables into the floor plan — so the floor screen,
  the table count, the `NeedsCleaning` queue and every future floor report inherit phantom entries,
  each needing an exclusion that is invisible until someone counts tables and gets the wrong number.

  The implementation slice includes the HTTP route and the edge-UI takeaway screen, because a domain
  change nothing can reach is the pattern this roadmap keeps catching.

### Security
- **`/internal/*` is no longer reachable from the internet** (`deploy/Caddyfile.d/site.caddy`).

  Two facts that were each documented and together were a hole. The three `/internal` handlers take
  no `HeaderMap` and perform **no authentication** — deliberately, because the code describes them as
  private-network-only. And the shared proxy block was a bare `reverse_proxy pos_cloud:8080` with no
  path allowlist, imported by every TLS posture. Nothing enforced the assumption the routes were
  written under.

  On a deployed instance an unauthenticated client could therefore:

  - `POST /internal/ingest` — inject arbitrary event envelopes for **any** tenant and store, feeding
    rollups, X/Z reports and ERP posting. (Not the edge's path: the edge publishes over NATS; this
    route exists only for the reconciliation re-push.)
  - `POST /internal/reconcile` — probe which event ids a store holds, and write fabricated
    reconciliation runs into the history the console shows.
  - `POST /internal/ota/report` — falsify `installed_version` and `self_test_ok` for any store,
    driving the Fleet and OTA views.

  The shared block now denies `/internal/*` with a `handle` block. `handle` blocks are mutually
  exclusive and matched most-specific-first, so the deny cannot be reordered behind the proxy by a
  later edit. It answers **404**, not 403 — a 403 confirms the route exists, and an internal surface
  should tell an unauthenticated caller nothing.

  **The `tls-modes` xtask gate now requires the deny**, so a fork cannot drop it silently; removing
  it fails the gate with a hint explaining why. Verified by mutation: with the deny removed the gate
  exits 1 and names the file, and it passes again once restored.

  No legitimate caller is affected: all three routes have zero external callers today —
  `/internal/ingest` is the cloud's own re-push, the edge reconcile caller is deferred by ADR-0078,
  and `/internal/ota/report` has no production caller at all. A store's own OTA reporting is being
  moved onto the authenticated store surface (`/sync/stores/{id}/…`), where the cloud resolves the
  tenant from the scoped key rather than trusting a body field; that is a port change and gets its
  own record.

### Changed
- **An OTA report can now say "no self-test yet"** — `UpdateReport.self_test_passed` and the
  `/internal/ota/report` field become `Option<bool>` / optional, implementing
  [ADR-0078](docs/adr/0078-sync-and-ota-closure.md) Amendment 1 (merged first, because this changes
  a port).

  Three states where there were two: `Some(true)` passed, `Some(false)` failed and should be
  surfaced, `None` never self-tested. That last one is the state most of a fleet is in most of the
  time, and it had to become expressible before the reporting loop could be honest — a report exists
  chiefly to say *which binary a store is running*, which matters from a store's first boot, and a
  two-state field would force the edge to invent a verdict it never earned.

  This makes the read model's existing `Option<bool>` reachable for the case the console was already
  written to render. `FleetStore.self_test_ok`, the nullable `self_test_ok` column, and "Not
  reported" in the OTA screen all pre-date this change; before it, that `None` only ever meant "has
  never reported at all", so a store that *had* reported could never show it. The console needed no
  change — verified, not assumed.

  Threaded through: `pos-ports`, the cloud route (`#[serde(default)]`), `OtaReportStore::record_report`,
  `store-postgres` (writing SQL `NULL`), the `cloud-sync-http` request (the field is omitted rather
  than sent as `null`, keeping the body byte-identical to the old shape for any store that does have
  a verdict), `pos-fakes`, and the `CloudSync` contract suite, which gains a sixth case binding every
  implementation to accept a report with no self-test.

  **The `/internal/ota/report` route had no tests at all**, which is how a two-state field survived
  into a three-state read model unnoticed; it now has a capturing fake and route tests for all three
  states plus the two refusals.

  Additive and no `PROTOCOL_VERSION` bump: `/internal` is unversioned, the field became optional
  rather than retyped, and an edge built before this posts the same body with its `true`/`false` read
  unchanged.

### Added
- **ADR-0078 Amendment 1 — an OTA report says "no self-test yet" instead of guessing**
  ([ADR-0078](docs/adr/0078-sync-and-ota-closure.md)). No behaviour change in this entry: the record
  lands first because it changes a **port** (`UpdateReport` in `pos-ports`).

  `UpdateReport.self_test_passed` becomes `Option<bool>`, and the `/internal/ota/report` field becomes
  optional. The original shaped `report()` around the case it was written for — a store that has just
  installed and self-tested — where a `bool` says everything needed. The reporting loop exposed the
  case it cannot express: the loop's primary value is telling the cloud **which binary each store is
  running**, which matters on every store from its first boot, long before any store has taken an
  update. For those stores there is no verdict, and a `bool` forces the edge to invent one — `true`
  shows a green **Passed** badge earned by nothing, `false` shows red on every healthy store in a
  fresh fleet, which trains an operator to ignore the column that exists to warn them.

  Reporting *only* once a self-test exists keeps the wire honest and loses the point: the installed
  version would stay empty for every store until it OTAs once, so the fleet view would be blank in
  exactly the state an operator most wants it.

  What makes this an amendment rather than a new decision is that **the read model was already right
  and unreachable**: `FleetStore.self_test_ok` is `Option<bool>`, the Postgres column is nullable, and
  the console already renders `null` as "Not reported" — but that `None` was only reachable for a store
  that had never reported *at all*. The three-state model existed end to end except in the one place
  that produces the value.

  Additive and no `PROTOCOL_VERSION` bump: `/internal` is unversioned, and the field becomes optional
  rather than changing type, so an edge built before this posts the same body and its `true`/`false`
  reads unchanged. The console needs no change — it renders the `null` it was already written to
  expect.
- **The store now remembers its OTA self-test across the restart an install performs**
  ([ADR-0048](docs/adr/0048-ota-rollout-model.md)'s rollback rule,
  [ADR-0055](docs/adr/0055-edge-ota-updater.md), roadmap v3 slice R5).

  `decide_rollout` puts one rule above every other — above even the kill switch: a device whose
  *running* version failed its self-test must revert. The fact that rule reads,
  `DeviceState.last_self_test`, was **process memory**, and an install deliberately restarts the
  edge. So the fleet's highest-precedence safety rule depended on the one fact a reboot destroys —
  the very reboot it exists to recover from. A box that installed a bad build, failed its self-test
  and restarted came back with no memory of failing, weighed itself against the same rollout, and
  was eligible to install the same bad build again. Migration `0006_ota_self_test` closes that loop.

  - **`OtaStateAuthority`** (`crates/pos-edge/src/ota_state.rs`) is where the store keeps its last
    self-test: one result per store, not a history, because the decision reads only the most recent
    verdict and the fleet's trail is the cloud's through `CloudSync::report`. Implemented for
    `store_sqlite::SqliteStore` over its single writer thread, with an `InMemoryOtaState` twin for
    tests and the example.
  - **`device_state(...)`** assembles the `DeviceState` a rollout decision is made against from the
    three places its facts actually live: `current` from this binary's own version (never a stored
    copy, which could disagree with it after a rollback — and the rollback rule compares the two),
    `ring`/`canary_bucket` from the cloud-published `device_ota` placement, and `last_self_test`
    from the authority. It returns `None` when the store has no placement, which is not an error:
    an unplaced store is eligible for no rollout.
  - **`ReleaseVersion` gains `Display`**, completing the pair with `ReleaseVersion::parse`, so a
    caller that has to store a version writes one spelling rather than assembling its own at each
    site. Two hand-rolled spellings that differ by a `v` are a version that parses back as `None`,
    and the rollback rule reads a `None` as "nothing to revert from".

  **Not a port, and deliberately so.** This follows the category `queue.rs` and `receipt.rs` already
  occupy — durable *edge-local* bookkeeping as a trait defined in `pos-edge`, implemented for
  `SqliteStore` over its public API, with an in-memory twin and its own additive migration. A port
  ([ADR-0026](docs/adr/0026-port-shapes.md)) is a boundary the domain crosses and a vendor could sit
  behind; nothing swaps in a different self-test store, and `store-postgres` has no reason to hold
  one, because the cloud learns a store's self-test through `CloudSync::report`
  ([ADR-0078](docs/adr/0078-sync-and-ota-closure.md)) rather than by sharing a table.
  [ADR-0091](docs/adr/0091-durable-edge-auth-state.md) gains a note narrowing its "must live in
  `pos-ports`" argument, which binds a trait the *adapter* implements and not one `pos-edge`
  implements *for* `SqliteStore`.

  **What this does not yet do, stated plainly:** nothing in production calls it. That is not new to
  this change — it is the condition of the whole OTA path, and this entry is the first to say so.
  `CloudSync::report` has no production caller (every `UpdateReport` in the tree is in `pos-fakes` or
  an adapter test), so the cloud's report route, the fleet read model, and the OTA console's progress
  pane are fed by nothing; and `UpdateInstaller` has no production implementation at all, so
  `OtaUpdater` cannot be constructed in the field even with the trusted keys and `SignedArtifact` in
  place. The install arm stays gated on that installer (the hardware gate ADR-0055 and ADR-0078
  already flagged) and on `POST /internal/ota/artifact`
  ([ADR-0088](docs/adr/0088-ota-artifact-hosting.md), blocked on object-storage credentials). The
  *reporting* arm needs neither, and is the next slice.

  No `PROTOCOL_VERSION` bump and no new API: one additive SQLite migration on the edge, and no wire
  format is involved.
- **The OTA rollout decision now has its inputs: the edge reads both config nodes, and the console can
  place a store in the rollout** ([ADR-0052](docs/adr/0052-ota-rollout-config.md) Corrections 1–2,
  [ADR-0048](docs/adr/0048-ota-rollout-model.md), roadmap v3 slice R5).

  `pos_core::ota::decide_rollout` is pure, total, and fully tested, and it had no production caller
  with anything to decide about. It needs three inputs, and none of them reached the running edge:

  - **`fleet_update`** — the rollout itself — has been published by the cloud since P9e-3, and
    `session_from_config` had no branch for it. Published, delivered, never read.
  - **`device_ota`** — where this store sits in the rollout — was validated by the cloud on publish
    and had no authoring route and no reader. Neither written nor read.
  - **`DeviceState.last_self_test`** still has no durable home; the rollback arm therefore cannot yet
    fire from real state. Named, not closed — see below.

  What lands here:

  - `EdgeSession` gains **`fleet_update: Option<FleetRollout>`** (the update plus the signing keys
    revocation has retired) and **`device_ota: Option<DeviceOtaAssignment>`** (this store's ring and
    canary bucket), read from the two nodes through the same `pos-core` `validate` methods the cloud
    runs before publishing — which is the shared-rules discipline ADR-0052 was built on, exercised on
    both sides for the first time.
  - **`PUT`/`GET /admin/config/ota/placement`** author and read the `device_ota` node on a store's
    Device config layer, beside the existing rollout levers: behind `console.ota.publish` for the
    write and `console.data.read` for the read, audited as `config.ota.placement`, and refused with
    every violation at once (`422`) when the ring names no ring or the bucket is past 99.
  - The **OTA console screen** gains a placement pane: it shows the published ring and bucket, or
    says plainly that an unplaced store installs nothing, and saves a new placement behind a
    confirmation. Typed client + `en`/`vi` strings included.

  Two design points worth stating, because both cut the opposite way from the other config nodes:

  - **An absent or invalid node leaves the previous value.** That is the never-blank rule every node
    in `session_from_config` follows, and here it is load-bearing rather than merely tidy: a store
    that *lost* its rollout or its placement would become eligible for nothing, so one bad publish
    would strand a fleet off security fixes. Halting a rollout is `halted` inside the node, never a
    deletion, so stopping the fleet never depends on a delete arriving.
  - **A placement has no default, and that is the safe choice, not a missing one.** `Ring::Lab` reads
    like the cautious placement and is the least cautious one available: lab is the first ring a
    rollout opens to, and `decide_rollout` exempts it from the canary ramp entirely, so a Lab default
    would install at the stage where an update has been proven on nothing. `Ring::Fleet` at bucket 0
    is the first fleet device in, at any ramp above zero. An unplaced store installing nothing is the
    safe end of that trade, and the console now says so instead of leaving an operator to notice a
    store that never updates.

  **Corrected in ADR-0052 rather than worked around:** "set at the device level" promised a
  granularity the delivery mechanism does not have. A config tree is keyed by `StoreId` and its Device
  layer is one document a store's terminals share, so a placement is **per store**, not per terminal.
  The wording is corrected rather than the mechanism, because per-store is the granularity a shop
  wants — a counter running two releases at once is a worse failure than a counter a week behind — and
  the ramp does its job at store granularity, which is the unit an operator watches and rolls back.

  **Still open, and deliberately not claimed closed here:** `DeviceState.last_self_test` needs a
  durable home across the reboot an install performs, and `POST /internal/ota/artifact` does not exist
  ([ADR-0088](docs/adr/0088-ota-artifact-hosting.md), gated on object-storage credentials). This slice
  closes the rollout *decision*'s inputs, not the install path.

  No `PROTOCOL_VERSION` bump: both config keys already existed on the wire and the added routes are
  `/admin`, which is unversioned.
- **ADR-0092 — the artifact trust chain, ahead of roadmap v3 slice R5**
  ([ADR-0092](docs/adr/0092-artifact-trust-chain.md)). No behaviour change in this entry: the record
  lands first because closing the gap changes a **port** — `CloudSync::fetch_update`'s return type —
  and because the other half of it is a security boundary that has one convenient wrong answer.

  `OtaUpdater` already fetches, verifies, then writes to disk, in that order. Two of the three things
  verification needs have **no production source at all**:

  - **The signature has no producer.** `UpdatePlan.signature` is what `OtaUpdater::verify` reads the
    claimed key id from, checks against the revocation list, and verifies. But `fetch_update` returns
    `Vec<u8>` — the artifact and nothing else — and no port method offers a `.minisig`. The only
    `UpdatePlan` in the tree is in `crates/pos-edge/tests/ota.rs`, which builds its own.
  - **The trusted keys have no source.** `OtaUpdater::new` takes `trusted_keys: Vec<PublicKey>` and
    nothing in production ever builds that vector. ADR-0047 said the keys are baked into the binary;
    the mechanism to bake them was never written.

  Neither is a live defect — R5 has not wired the updater in, so the whole thing has zero production
  callers — and that is precisely the argument for settling it now. R5 is otherwise where these get
  discovered one at a time, under pressure to make it compile, and the easiest way to fill a
  `trusted_keys` vector at that moment is from the config tree, which is already parsed and already
  in `EdgeSession`. That would hand a cloud the ability to choose the key its own artifact is
  verified against.

  What the record settles:

  - **`fetch_update` returns `SignedArtifact { bytes, signature }`.** Not for convenience — so that
    **skipping verification stops being expressible**. Two independent calls let a caller hold 30 MB
    of executable bytes with no signature and no complaint from the type system; returning the pair
    means the only way to get the bytes is to also be handed the thing that judges them.
  - **On the wire the artifact stays the raw body and the signature rides a response header.** A
    detached minisign signature is a few hundred bytes and the artifact is tens of megabytes, so
    wrapping both in JSON would cost roughly +33 % on every download by every store in every ring to
    move a payload that fits in a header. The `HttpTransport` seam's `HttpResponse` gains an additive
    `headers` field. (The record said base64; implementing it found there is no base64 crate in this
    workspace, so honouring the word would have meant a new dependency — itself an ADR-first change.
    Hex needs nothing new, costs a few hundred extra bytes on a signature, and leaves the
    header-versus-body argument untouched. Recorded as ADR-0092 Correction 1.)
  - **Trusted keys are compiled in through `option_env!`** — the same mechanism R1b used for the
    release version, so no build script and no new dependency — **and no runtime path may supply
    them.** Stated as a prohibition rather than a preference: a key taken from the cloud-published
    config tree is a key an attacker controlling the cloud can choose, which makes the check verify
    the attacker's artifact against the attacker's key. A trust anchor cannot live inside the channel
    it protects.

  **Upgrade note** None yet — nothing is wired. A fork will need to set the trusted-key build
  variable, and a binary built without it cannot install an update, which is the correct failure for
  an updater with no trust anchor. `PROTOCOL_VERSION` is unaffected: `/internal` is unversioned and
  the header is additive. `arch` is *not* re-decided here — [ADR-0088](docs/adr/0088-ota-artifact-hosting.md)
  Correction 2 already settled it as an additive request field the adapter fills from its own build
  target.

- **A locale that falls behind `en` now fails the build instead of falling back silently** (roadmap
  v3 slice **Q2**, #119). `scripts/i18n-parity.mjs`, mirrored into both front-end roots and wired
  into `pnpm build`, requires every catalogue to carry exactly the key set `en.json` carries.

  **The scope is one property, deliberately.** A *bad* key is already impossible: `t()` takes
  `MessageKey = keyof typeof en`, so a typo is a type error and `tsc --noEmit` rejects it ahead of
  this. What the type system cannot see is the other catalogues — a key added to `en.json` and
  forgotten in `vi.json` type-checks perfectly and *works*, because the runtime falls back to
  English. So it ships, and a Vietnamese operator reads an English string mid-shift with nothing
  failing anywhere to say so. That fallback is the right runtime behaviour — a blank or a raw key on
  a till would be worse — which is precisely why the check has to happen at build time: the safety
  net is silent by design. The reverse direction is caught too, since a key `en` has dropped is
  usually the leftover half of a rename.

  Both apps are in parity today (1233 keys in the console, 90 in the operator UI, exact in both
  directions), so this is a drift guard rather than a fix. It was verified by breaking it: deleting
  one key and adding one stray key each fail with the offending key named.

### Changed
- **The edge's trusted signing keys now have a source, and only a build-time one**
  (roadmap v3 slice **R5**, [ADR-0092](docs/adr/0092-artifact-trust-chain.md) part 2).
  `pos_edge::trusted_keys()` reads `POS_EDGE_TRUSTED_KEYS` through `option_env!` — the mechanism R1b
  used for the release version, so no build script and no new dependency — and `release.yml` now
  fails before it builds if the variable is unset, beside the existing check for the signing key.

  **Nothing could build that vector before.** `OtaUpdater::new` took `trusted_keys: Vec<PublicKey>`
  and the only source in the tree was a test fixture. ADR-0047 said the keys were "baked into this
  binary"; the baking was never written.

  **The shape enforces where a key may come from, not a comment.** `trusted_keys()` takes no
  arguments and the parser is private, so there is no public function in `pos-edge` that turns a
  runtime string into a `PublicKey`. That matters because the easiest place for R5 to have found a
  key list is the cloud-published config tree — already parsed, already in `EdgeSession` — and a key
  taken from there is a key an attacker controlling the cloud can choose, making the signature check
  verify *their* artifact against *their* key. A trust anchor cannot live inside the channel it
  protects. `docs/fork-checklist.md` previously said to "record it in the fleet's OTA trust
  configuration", which reads like exactly that; it now names the build variable and says why.

  **The format is minisign's own**: each entry is the second line of a `minisign.pub` file verbatim
  (base64 of `Ed` ‖ key id ‖ key), comma-separated for two keys so a compromised one can be retired
  — ADR-0047's reason for keeping two. So a fork pastes what the tool produced,
  `POS_EDGE_TRUSTED_KEYS="$(sed -n 2p minisign.pub)"`, with no hand conversion on a once-per-fork
  step where a mistake yields a binary that cannot update and says so only at the first rollout.

  A binary with no keys returns `TrustedKeyError::NotBakedIn` rather than an empty vector, so the
  wiring cannot mistake "no anchor" for "nothing to check"; an updater with no trust anchor must
  refuse to install.

  **Upgrade note** A fork must set the `POS_EDGE_TRUSTED_KEYS` **repository variable** (not a
  secret — a public key is not secret, and a variable is visible in the run log where an operator can
  see which anchor a release was built against). `release.yml` refuses to build without it. Nothing
  else changes: `OtaUpdater` still has no production caller until R5.

- **The edge can no longer obtain an update artifact without the signature that judges it**
  (roadmap v3 slice **R5**, [ADR-0092](docs/adr/0092-artifact-trust-chain.md)).
  `CloudSync::fetch_update` returns a `SignedArtifact { bytes, signature }` instead of `Vec<u8>`, and
  `UpdatePlan` **loses** its `signature` field.

  Dropping the field is the actual fix. It was `signature: &'a Signature`, and nothing in production
  could fill it in: the port returned artifact bytes alone, so the only `UpdatePlan` ever built was
  in `pos-edge`'s own tests. The answer was not to find it a producer but to delete it — a signature
  belongs to the artifact, arrives with it, and a plan assembled *before* the fetch has no business
  claiming to know it. Keeping both would have been worse than either: two signatures with no rule
  for which wins.

  What the pairing buys is that **skipping verification stops being expressible**. Two independent
  calls let a caller hold tens of megabytes of executable bytes with no signature and no objection
  from the type system; every such site then rests on somebody remembering the rule. Now the only
  way to get the bytes is to also be handed the thing that judges them, so `OtaUpdater::install` has
  no arrangement in which unverified bytes reach `apply`.

  On the wire the artifact stays the raw body and the signature rides `X-Pos-Artifact-Signature` as
  lowercase hex, following the existing `X-Pos-Webhook-Signature` naming. `HttpResponse` gains an
  additive `headers` field to carry it. A `2xx` carrying no signature header is a **retryable
  failure**, not a successful fetch — bytes with nothing to judge them are unusable, a proxy
  stripping a header is the likely cause, and it must never read as permission to install.

  The `CloudSync` contract suite gains the obligation, so it binds every future adapter rather than
  just this one: a fetched artifact arrives with a non-empty signature, and it is the one published
  beside those bytes. Verified by mutation — a fake serving an empty signature fails exactly that
  case. `FakeCloudSync` now serves a signature that genuinely verifies under `FakeSigner`, and gained
  `serving_signature` so a test that wants a hostile cloud has to say so; the edge's untrusted-key
  and swapped-blob tests were rewritten to use it, which is a better model of the threat anyway — the
  attacker is the host serving the artifact, not the code assembling the plan.

  **Upgrade note** `PROTOCOL_VERSION` unchanged: `/internal` is unversioned and the header is
  additive. Nothing is wired yet — `POST /internal/ota/artifact` still does not exist, so no
  deployment behaviour changes. A fork with its own `CloudSync` adapter must return the pair and
  serve the header; the contract suite will tell it so.

- **Cloud failures answer one machine-readable error shape, starting with every shared error
  responder** (roadmap v3 slice **Q3a**). A refusal now carries the AIP-193 envelope
  `pos-proto` has always defined — `{"error":{"code":503,"status":"UNAVAILABLE","message":"…"}}` —
  instead of a bare line of text. `api_error` is the only way `pos-cloud` builds one and it derives
  the HTTP status from the body's own `ErrorStatus` through `ErrorStatus::http_code`, so the status
  line and the body cannot disagree.

  **This slice converted the shared responders, which is where the leverage is**: the thirty
  per-domain helpers (`catalog_error_response`, `config_store_error_response`,
  `admin_service_unavailable`, each `*_entropy_unavailable`, `activation_refused`,
  `too_many_login_attempts`, …) carry **258 call sites** between them, so thirty one-line edits moved
  258 response paths. The inline validation refusals written at each handler are the next slice, and
  they are separate because they are the ones that will carry field-level `details`.

  **Mid-conversion the surface is mixed, and that is safe by construction, not by luck.** Nothing
  consumes the shape: `cloud-sync-http` maps cloud failures on HTTP status alone and never parses a
  response body, and the console's `failure()` now reads the envelope, the config publish's
  `{"violations":[…]}` and raw text alike. `ApiError` gained `canonical` (the status token) and
  `details`, so a caller can branch on `ALREADY_EXISTS` versus `FAILED_PRECONDITION` — two
  conditions that share `409` and cannot be told apart from the number.

  **Upgrade note** The `/v1` and `/sync` error *bodies* change shape and their `content-type`
  becomes `application/json`; status codes are unchanged. There is no version negotiation because
  there is no external consumer to negotiate with, and `PROTOCOL_VERSION` is untouched — the wire
  format for events and config is not involved. A fork that had been reading `/v1` error text should
  read `error.message`, and can branch on `error.status` instead of the HTTP number.

- **The console's login page no longer downloads the whole console** (roadmap v3 slice **Q2**, #119).
  Every screen behind the auth guard is now a `lazy()` route chunk; the three public screens
  (`Login`, `Setup`, `AcceptInvite`) stay in the initial bundle because they are all an
  unauthenticated visitor can reach.

  **Initial JS: 540.57 kB → 268.15 kB (−50.4%); gzipped 131.47 kB → 76.67 kB (−41.7%)**, split into
  37 route chunks with the largest (the Catalog shell) at 45.58 kB. Before this, anyone opening the
  login page paid for all thirty-one screens first — the Reports charts, the Layout editor, every
  Catalog sub-screen — before they could type a password. Vite had been printing its "chunks are
  larger than 500 kB" warning and recommending exactly this; the warning is now gone.

  **Upgrade note** None for operators. A fork adding a console screen should follow the `lazy()`
  pattern in `dashboard/src/App.tsx` rather than importing it eagerly, or the screen rejoins the
  initial chunk and quietly gives back part of this.

### Fixed
- **A `PortError` that was the caller's fault would have been reported as the server's** (roadmap v3
  slice **Q3a**). `error_response` mapped four statuses — `NOT_FOUND`, `ALREADY_EXISTS`,
  `PERMISSION_DENIED`, `UNAUTHENTICATED` — through a catch-all arm to `500`, telling a client the
  cloud had broken when its own request was at fault and inviting a retry that could never succeed.

  **Latent rather than live, and recorded that way on purpose.** The function has exactly one caller
  today (`/internal/ingest`), and none of those four statuses is reachable from it, so no response
  was actually wrong. The fix is to derive the code from `ErrorStatus::http_code` and delete the
  hand-written match, so the *next* caller cannot inherit the trap; a test walks every canonical
  status through `error_response` and fails if the match ever returns.

- **`i18n-lint.mjs` was a duplicated file that nothing kept in step** (#119). It is copied verbatim
  into `ui/scripts/` and `dashboard/scripts/`, but unlike `wcag-contrast.mjs` it was never listed in
  the `mirrored-files` gate — so the two copies were byte-identical only by luck, which is the exact
  silent drift that gate's own documentation describes. Both it and the new `i18n-parity.mjs` are now
  listed.

- **`IntakeLedger` is a registered port at last, with a suite both its implementations pass**
  ([ADR-0064](docs/adr/0064-edge-order-in.md) amendment, #118). It was built as a port and called one
  by its own ADR, but it never got a `PortName` variant — and everything downstream of that registry
  quietly skipped it: no shared contract suite, so the two implementations were **never checked
  against each other**; no row in `docs/architecture.md` §5, so the authoritative port list was wrong
  by one; and its failures reported under `PortName::OrderIn`, merging two seams into one metric
  label.

  It now has the variant (the nineteenth), the `intake_ledger` label, a row, and a six-case suite
  that `pos-fakes` and `store-sqlite` both run. The case that earns its place is
  `a_repeat_key_is_refused_at_commit`: the ledger's whole purpose is that a marketplace's retry
  cannot open a second order and charge a guest twice, and that guarantee lives in a **commit**, not
  a row — so the suite drives the transaction boundary itself and checks that the second write loses
  with `AlreadyExists` while the first record stands. Its mirror,
  `an_uncommitted_record_is_not_found`, covers the direction that matters after a crash: a
  rolled-back intake must not resolve, or a retry would be told "already handled" for an order that
  was never opened, and the food would never be made.

  **Why it went unnoticed, stated plainly.** The `every_port_has_a_suite` guard iterates
  `PortName::ALL`, so it can only check ports that were registered — it can never be the thing that
  catches an unregistered one. The control that should have caught this is the rule that a new port
  needs an ADR first and a reviewer who reads it. That is a process control, not a test, and it is
  the one that failed. The blind spot is now written down in three places rather than fixed and
  forgotten.

  **Upgrade note** None; no wire, schema or migration change. One observable difference: errors from
  the intake ledger now carry `intake_ledger` rather than `order_in` as their port label, so a
  dashboard or alert filtering on `order_in` for ledger failures needs the new label. The
  queue-number allocator keeps `order_in`, which is the seam it belongs to.

- **The acceptance suite now drives the router the shipped binary serves** (roadmap v3 slice **Q1**,
  the v1.0 gate, #117). `pos_edge::compose` is `serve` minus the socket — the same state, the same
  router, the same background loops — and `crates/pos-edge/tests/acceptance.rs` walks a store's whole
  day through it: pair a device over `/api/pair`, sign a person in with a PIN, seat a table, order,
  fire to the kitchen, bump the ticket, read what the table owes, open the bill, settle it, cycle the
  table to clean; open a cash shift, count it blind, close it; and accept a relayed takeaway order.

  **Why this is not one more HTTP test.** Every other suite in the crate builds its router by hand
  (`http::domain_router(...)`), so it proves the *routes* work and says nothing about whether `serve`
  **mounts** them. That gap is the most expensive one in this tree's history: seven slices shipped
  code that was written, unit-tested and unreachable from the running binary, and an eighth turned up
  the same week. It is measured, not asserted — deleting the domain-router merge from `serve` fails
  four of these seven cases, while the hand-built suite passes all of its own.

  The negative cases are part of the point: an unpaired host on the LAN is refused every domain route
  (`401`), and a paired device with nobody signed in may sign someone in and nothing else (`403`) —
  the two-gate split of [ADR-0084](docs/adr/0084-device-authentication.md), checked where the binary
  actually serves it rather than where it is implemented. The store runs on `pos-fakes` with no
  `cloud_url`, which is the acceptance condition itself and not a shortcut: a shop with its cable
  unplugged keeps trading ([ADR-0001](docs/adr/0001-offline-first-store-autonomy.md)).

  **Upgrade note** None. `compose` is additive and `serve`'s signature and behaviour are unchanged;
  it now calls `compose` and then binds. A fork embedding the edge can build the composed router
  without binding a port.

- **A released binary now reports which release it is** (roadmap v3 slice **R1b**, #116). The release
  workflow stamps the tag it already validates into the build as `POS_EDGE_RELEASE_VERSION`, and
  `pos_edge::version` reads it through `option_env!` with `CARGO_PKG_VERSION` as the fallback. Before
  this, `crates/pos-edge/Cargo.toml` was `version = "0.0.0"` and nothing wrote the tag at build time,
  so **every artifact reported the same version**: `/healthz` said `0.0.0`, every store told the cloud
  `0.0.0` over `CloudSync::report` ([ADR-0078](docs/adr/0078-sync-and-ota-closure.md)), and the OTA
  progress model could not tell one release from another — a rollout's progress was a bar that never
  moved.

  **A hand-built binary still says `0.0.0`, and that is the honest answer** — it is not a release, and
  `0.0.0` sorts below every published version, so such a box is eligible for any update rather than
  wrongly believing itself current. No build script and no new dependency: a `git describe` script
  would also need `.git` in the build context, which the Docker build does not have.

  **The tag's `v` is stripped, and that is load-bearing.** `ReleaseVersion::parse` splits on `.` and
  parses `u16`s, so a leading `v` makes it return `None`, and the cloud publishes `target_version` in
  the same bare form. A test asserting the *compiled-in* value parses fails the build — not the fleet —
  if a fork changes the workflow's expression.

  **Upgrade note** No wire, schema or configuration change; `PROTOCOL_VERSION` is unchanged. A fork
  that builds its own artifacts outside `.github/workflows/release.yml` should export
  `POS_EDGE_RELEASE_VERSION=X.Y.Z` (no `v`) in its build, or every store it runs will report `0.0.0`
  and its OTA rollouts will show no progress.

- **A restart no longer unpairs the store, and no longer signs everyone out**
  ([ADR-0091](docs/adr/0091-durable-edge-auth-state.md), roadmap v3 slice **S0d-2**). The half that
  changes behaviour. `Pairing` and `Sessions` now write through to the `DeviceRegistry` S0d-1 added
  and load from it at boot, so a power blip, an OTA install (ADR-0055 restarts the edge on purpose)
  or a `systemctl restart` stops being the Friday-evening scene it was: every tablet unpaired at
  once, an operator walking to the console to read six-digit codes while the queue builds.

  **Reads stay in memory.** The gate on the front of every request answers from a map, as it always
  did; only *changes* touch the database. Loading is fatal if the registry cannot be read — starting
  with an empty table would silently unpair a store that *is* paired, and an operator would then
  re-pair every till to fix a problem that was never theirs.

  **The edge now holds no device token anywhere.** The live map is keyed by digest, like the durable
  table — it has to be, because a restart can only restore what was stored — so a token exists only
  for as long as it takes `redeem` to hand it to the device that will keep it. That is a property
  ADR-0091 asked for on disk and this gets in memory too.

  **Sign-in survives a restart, bounded by `sign_in_idle_timeout_minutes` (default 30).** Past the
  window a device is reported signed out and gets the same `403` an unsigned one gets. The rule is a
  pure comparison and it **fails closed on a clock that misbehaves**: a negative interval (the clock
  went backwards) or an implausibly large one both expire the session, because no SNTP poll runs on
  the edge today and an NTP daemon or a person with `date` can step the host clock either way. A
  clock that jumps must never be a way to hold a session open. `0` is refused at startup rather than
  accepted — a zero window signs a device out between its own two requests.

  **Revocation is now reachable**, since a restart is no longer doing it by accident: `POST
  /api/pair/revoke` retires one device or all of them (the break-glass that reproduces the old
  behaviour deliberately), and `GET /api/pair/devices` reports the count and whether it is durable —
  an operator planning a reboot mid-service needs to know whether it will cost them the fleet. Both
  sit behind the paired-device gate: as strong as pairing and no stronger, because the edge has no
  operator identity offline.

  Write ordering is deliberate and **not** uniform, which is worth knowing when reading the code.
  Pairing and sign-in record durably *before* they are believed, so a failure refuses rather than
  handing out a credential the box may forget. Sign-*out* is the opposite — memory clears first and
  unconditionally — because a half-failed sign-out must leave the till locked on this box; an
  operator told a device is signed out when it is not is the worse error.

  **Upgrade note:** nothing to do. An upgrading store starts with both tables empty, which is exactly
  today's post-restart state, so the first boot behaves as before and the *second* is when the
  improvement shows. A fork that composes no registry (the on-fakes example, the test suite) keeps
  the pre-S0d in-memory behaviour, and there is a test that says so. One new setting,
  `sign_in_idle_timeout_minutes`; one new dependency on `pos-edge`, `sha2` 0.11 — the same
  RustCrypto line `pos-cloud` and `blob-garage` already pin, so no new crate enters the lock.
- **`DeviceRegistry` — the eighteenth port, so pairing and sign-in can survive a restart**
  ([ADR-0091](docs/adr/0091-durable-edge-auth-state.md), roadmap v3 slice **S0d-1**). The port half
  of S0d: the trait, its contract suite, the fake, and the real `store-sqlite` adapter with an
  additive migration. **No behaviour change in `pos-edge` yet** — the edge still keeps both tables
  in process memory until S0d-2 wires this in. Split the way `CloudSync` was
  ([ADR-0053](docs/adr/0053-cloud-sync-port.md): port+suite+fake, then adapter, then edge wiring),
  because the port and the wiring fail in different ways and reviewing them together hides both.

  **The port never receives a device token.** It takes a `TokenDigest` — a SHA-256, computed by the
  caller — so an implementation cannot leak a credential it was never handed, and a stolen `pos.db`
  yields digests, which cannot be presented to the gate. `TokenDigest`'s hex encoding is hand-rolled
  because `pos-ports` is backbone and carries no hash dependency; the hashing itself belongs where
  the token already exists.

  **The idle timeout is deliberately not in the port.** It stores `last_seen_at` and hands the
  session back; the caller decides expiry. So the rule is one pure comparison tested without a
  database, and every adapter behaves identically because none of them knows it. What the suite
  *does* pin is that `touch_session` actually moves the stored instant — a timeout reading a value
  nothing updates would expire every session on schedule regardless of use.

  Thirteen contract cases, passing against **both** the in-memory fake and real SQLite, so
  "swappable" is checked rather than claimed. The two that matter most are about the tables
  *disagreeing* rather than either one working: revoking a device must clear its sign-in (a session
  belonging to no paired device is unreachable state a later feature could read as live), and a
  revoked token must stop resolving (an implementation that stores but never forgets would pass
  every other case and still be unusable). `FakeDeviceRegistry` keeps two maps rather than one so
  the first of those has something real to get wrong.

  **Migration:** additive `0005_device_registry.sql` — two tables, nothing renamed or removed. An
  upgrading store starts with both empty, which is exactly today's post-restart state, so the
  upgrade needs no special step and cannot be worse than the status quo. `ON DELETE CASCADE`
  documents the revoke invariant and the adapter also deletes the session explicitly inside one
  transaction, because the cascade depends on a `PRAGMA` and the invariant should not.

  **Port-list bookkeeping**, following the pattern ADR-0053 set: `PortName::DeviceRegistry`,
  `docs/architecture.md` §5 gains a row and now reads *eighteen*, and `pos-contract-tests`' registry
  gains the suite the `every_port_has_a_suite` gate requires.
- **The ADR index is complete again, and records ADR-0091** (`docs/adr/README.md`). The table stopped
  at 0066 and was **25 records behind** — every decision from the multi-admin console (0067) through
  the TLS postures (0090) existed as a file and was reachable only by guessing its filename. For a
  repository whose whole premise is being forked and read by someone who did not write it, the index
  is the entry point, so this is a defect rather than untidiness. All 90 records are now listed with
  their titles and statuses.
- **ADR-0091 — durable edge auth state, ahead of roadmap v3 slice S0d**
  ([ADR-0091](docs/adr/0091-durable-edge-auth-state.md)). No behaviour change in this entry: the
  record has to land first, because persisting the edge's pairing and sign-in tables needs an
  **eighteenth port**. Every adapter in this tree depends on exactly `pos-proto` and `pos-ports` and
  nothing else, so a trait `store-sqlite` implements cannot be defined in `pos-edge` — the dependency
  runs the other way — which makes S0d a port-list change and therefore an ADR-first one.

  The record settles the three things persistence *changes*, none of which are plumbing:

  - **The device token is stored as a SHA-256 digest, never in the clear.** A token is a bearer
    credential, and `pos.db` is far easier to walk off with than a running process's heap — which is
    exactly what changes when this becomes durable. A plain digest suffices and a password KDF would
    be wrong: the token is 128 bits from the OS CSPRNG, so there is no dictionary to run and no salt
    to add, and lookup stays one indexed equality read on the gate every request crosses.
  - **Revocation becomes explicit, because it stops being accidental.** Today a restart revokes every
    token and every sign-in. Making them durable removes that, so the port gains a revoke path —
    per-device, plus a revoke-all that reproduces the old restart behaviour on purpose. It works
    offline, because a store noticing a missing tablet cannot be told to wait for the cloud.
  - **Both tables survive a restart, guarded by a 30-minute `sign_in_idle_timeout`.** Durable pairing
    with volatile sign-in was the safer option in isolation and is rejected: it halves the outage
    instead of ending it, since staff would still all re-enter PINs at once on a mid-service reboot.
    The timeout carries the risk that decision creates — a till carried off while signed in as a
    manager — and idleness is the only signal available for it without the cloud.

  One thing worth knowing for anyone reading the timeout code later: **no SNTP poll runs today.**
  `pos-edge`'s `sntp` module has no production caller, which [ADR-0073](docs/adr/0073-alerting.md)
  already recorded for the drift signal, so the clock behind this timeout is the host OS clock and a
  daemon or a person can step it either way. The timeout is therefore a difference between two stored
  instants that **expires the binding on a negative or implausible result** rather than extending it:
  a clock that jumps must never be a way to hold a session open.
- **E7 — the store event bus is reachable: `nats` publishes `4222` with TLS**
  ([ADR-0089](docs/adr/0089-edge-event-bus-transport.md), roadmap v3 slice **E7**). The outbox
  publish E3 wired into `serve()` finally has somewhere to go. Until now `nats` sat on an
  `internal: true` network with no published port and no proxy route, so `POS_EDGE_NATS_URL` had no
  valid value at all — and the failure was silent by design, because an unreachable broker is
  non-fatal so the store keeps trading. A fleet in that state looked healthy from the till and empty
  from the cloud: rollups, revenue and product-mix reporting, X/Z aggregation and reconciliation were
  all reading nothing.

  **The port never opens without TLS, and that is now mechanical rather than a runbook promise.**
  `bootstrap.sh` publishes `4222` on `0.0.0.0` with a `tls` block **only** when
  `deploy/secrets/tls/` holds a certificate — the path ADR-0090 established — and binds it to
  loopback otherwise, saying which case applies and why. On the ACME modes a first deploy therefore
  leaves the bus closed, because Caddy has not issued yet; the next bootstrap opens it. Publishing
  the port makes the broker internet-facing with nothing in front of it but its TLS and its token,
  which is the honest cost of terminating TLS *at* NATS — the property that makes per-store client
  certificates possible later.

  `secrets/nats.conf` is now **rewritten on every run with the existing token carried across**,
  because whether the port is TLS-wrapped follows from the posture and can change on a redeploy. A
  run that cannot read the existing token **refuses** rather than minting a new one: rotating it
  would break every store's publish and the cloud's own ingest cursor in the same instant.
  `TLS_RELOAD_SERVICES` now defaults to `nats`, so `tls-export.sh` SIGHUPs the broker on a renewal
  and it re-reads its certificates — without that hook a consumer serves a stale certificate until
  expiry and then goes dark silently, weeks later.

  **Still unverified, and said so rather than claimed:** whether a published port works on a service
  attached only to an `internal: true` network. It is host→container DNAT, which *should* be
  unaffected, and that cannot be proven from a repository. The runbook has the one-command check
  (`nc -zv <host> 4222`) and the recorded fallback (attach `nats` to `frontend`, at the cost of
  egress it does not need). Per-store mTLS remains its own slice; when it lands the `tls` block gains
  `verify_and_map` and a **private** client CA, never a public one (debate D25).

  **Upgrade note.** Set `POS_EDGE_NATS_URL=tls://:<token>@<your DOMAIN>:4222` in each store's
  mode-0600 `env` file, with the token from `deploy/secrets/nats.conf`; the `tls://` scheme is what
  makes the client require TLS. Add the `tls-export.sh` cron line first if you have not, or the port
  will not open. Restrict `4222` at the host firewall where your stores' addresses are knowable.

### Fixed
- **Pinned the comparison the whole fleet's ability to update rests on** (roadmap v3 slice **R1b**,
  #116). `decide_rollout` gates on `update.target <= device.current`, and both sides arrive as *text* —
  the tag stamped into the binary, and the `target_version` string the cloud publishes. The comparison
  is numeric today because `ReleaseVersion` is three `u16`s with a field-order `Ord`, but nothing tested
  it at a boundary where that matters. There are now cases at 9→10, 1.9→1.10 and 1.1.9→1.1.10, in both
  directions and through `decide_rollout` itself: the exact places where a version "simplified" to a
  string would compile, pass every other test, and silently strand every store at the first two-digit
  component (as text, `"10.0.0" < "9.9.9"`). Not a live defect — a missing test under a live assumption,
  found while wiring the stamp that makes the comparison start seeing real values at all.

- **A NATS token written into the broker URL was silently discarded, so the documented way to arm
  the cloud's ingest cursor could not work** — and the event bus of
  [ADR-0089](docs/adr/0089-edge-event-bus-transport.md) would not have authenticated either.

  `async-nats` 0.50 builds its `CONNECT` frame from `ConnectOptions::auth` alone.
  `ServerAddr::username`, `password` and `has_user_pass` are public accessors with **no caller
  inside the crate**, so `nats://:TOKEN@host:4222` presents no credential at all — while
  `bootstrap.sh` generates a `nats.conf` with `authorization { token: … }` enforced, which then
  refuses the connection. That URL form is exactly what `bootstrap.sh` writes into `cloud.toml` as
  the instruction for turning the feed on, what the new-store wizard emits as `POS_EDGE_NATS_URL`,
  and what ADR-0089 rested on when it said the adapter needed no change.

  `link-nats` now lifts credentials out of the URL and presents them through `ConnectOptions`
  (`src/endpoint.rs`). Three details are load-bearing: a **schemeless** address like
  `127.0.0.1:4222` or `nats:4222` — what the tests and `compose.yml` use — is passed through
  untouched, because `Url::parse` would read its host as a *scheme*; the credentials are **removed
  from the address**, so a token cannot ride into a connection error or `Debug` output; and
  percent-escapes are **decoded**, since a userinfo field must encode `@`, `:` and `/` to parse at
  all and the raw field would present a mangled secret. Both spellings work — `nats://TOKEN@host`
  and `nats://:TOKEN@host` — as does `nats://user:pass@host`.

  **Why no test caught it:** the integration suite runs NATS with *no authorization at all* and
  connects with a bare address, so it exercised the one posture no deployment uses. CI now starts a
  **second, token-enforcing** broker, and the new `tests/auth.rs` proves the token in the URL
  authenticates *and* that the same broker refuses the address without it — the second case exists
  so a green first case cannot be a server that would have accepted anything.

  Same shape as the rest of this release's findings: code that was written, tested and could not
  work in production, with a document telling operators to use the broken form. ADR-0089 carries a
  correction recording that its "no code change in `link-nats`" consequence was wrong, and why.

### Security
- **`/ws` now requires the paired device token: any host on the store LAN could read the whole
  committed-event stream** ([ADR-0084 amendment](docs/adr/0084-device-authentication.md), roadmap v3
  slice **S0c**, #112). S0a gated every domain route on the pairing token and S0b added employee
  sign-in on top, but `/ws` sat on the ungated infrastructure router the whole time. It streams every
  event the store commits — orders, bills, settlements, shift closes — so a laptop plugged into the
  store switch read the trading day without sending a command or presenting a credential. ADR-0084
  filed this under a later integration slice (W6·B6.1) as "the read-side eavesdrop"; that was the
  wrong urgency, and the tree audit reclassified it.

  `/ws` is now a one-route sub-router behind `require_paired_device_ws`, which refuses an absent,
  malformed, or unknown token with the same `401` every other gate gives — and a refused connection
  never subscribes to the fan-out, so it cannot receive even one frame. The rest of the
  infrastructure router stays open by necessity: `/healthz` answers an unauthenticated probe,
  `/api/pair` is how a device *gets* a token, and the asset fallback serves the app that does the
  pairing.

  **The token reaches the gate through `Sec-WebSocket-Protocol`**, because the browser `WebSocket`
  API cannot set request headers and so `Authorization` is unavailable to the store UI on an upgrade.
  The client offers `[pos-edge.v1, <token>]`; the server selects **only the name**, so the credential
  never travels back in the handshake response. A query parameter was the obvious alternative and was
  rejected: the edge logs the request path on every request, so `/ws?token=…` would write a device
  credential into a log. `Authorization` still works and is tried first, for a non-browser consumer
  that can set it.

  Why it survived two auth slices: the fan-out tests connected **without** a token and passed, so the
  path was proven to work and never proven to be closed. They now pair first, and three new cases
  pin the refusals — an unpaired host, a well-formed token that was never issued, and the bearer
  path.

  **Upgrade note:** an operator UI older than this edge cannot open `/ws` — it sends neither a
  subprotocol nor a header — and will show as disconnected. The UI and the edge ship together in one
  OTA artifact, so this only affects a hand-mixed pair. The in-memory token set is unchanged, so an
  edge restart still drops every socket until each device re-pairs; making that durable is **S0d**.
- **The `/admin/login` rate limit was bypassable, and is now keyed on an address the client cannot
  choose.** `client_ip` read the **leftmost** hop of `X-Forwarded-For` and that value fed the sliding
  window as its only key (`ip:…`), so an online password guesser could send a fresh fake
  `X-Forwarded-For` per attempt, land in a fresh bucket every time, and never meet the throttle at
  all — the defence read as working and was not.

  Caddy's `reverse_proxy` **appends** to the chain rather than replacing it, so a request arriving with
  `X-Forwarded-For: 1.1.1.1` reaches the app as `1.1.1.1, <the address Caddy observed>`. Everything
  left of the hops we appended ourselves is attacker-supplied text, not an address. `client_ip` now
  counts back from the right by `TRUSTED_PROXY_HOPS` (one: the TLS-terminating Caddy of ADR-0044), and
  the `X-Real-IP` fallback is removed — single-valued and client-settable, it had no chain to count
  back along, so honouring it reopened the same hole.

  Same shape of defect as this release's `cloud_url` bug, for the third time in one pass: **the
  function's own doc comment asserted the value was "never trusted for authorization"**, which was
  true when it only decorated the session list and stopped being true when G1 slice 5 made it the
  rate-limit key. Nobody revisited the comment. Seven new unit tests pin the rule directly, including
  the forged-leading-hop case that would have caught this.

  Two consequences worth knowing: an absent header is now `None`, which callers key as one shared
  bucket — over-throttling a direct caller rather than exempting it, so a missing header is not a way
  through either. And the session list stops being able to display an attacker-chosen address.

  **Note for a fork that puts its own load balancer in front of Caddy**: it has more than one trusted
  hop and must raise `TRUSTED_PROXY_HOPS`, which debate D24's `TLS_MODE` work makes configurable.
  Until then such a deployment under-counts and throttles by the wrong address — the safe direction to
  be wrong in, but wrong.

### Added
- **`TLS_MODE` — four TLS postures, so a fork can bring its own certificate or terminate upstream**
  ([ADR-0090](docs/adr/0090-tls-postures.md), roadmap v3 slice **E7a**). The implementation of the
  record below.

  `TLS_MODE` is `acme-http01` (the default when unset, so an existing cell is unchanged) ·
  `acme-dns01` · `byo-cert` · `external`. Each is a committed Caddyfile under `deploy/Caddyfile.d/`
  importing one shared `site.caddy`, and `bootstrap.sh` installs the selected one as a **generated**
  `deploy/secrets/Caddyfile` — nothing overwrites a version-controlled file any more, the proxy block
  exists once instead of per mode, and the chosen posture is recorded in `secrets/caddy.env` and
  printed on every run. CI builds the Cloudflare-plugin Caddy image only for `acme-dns01`; every
  other posture ships the stock image.

  **`deploy/secrets/tls/` is now the certificate path in every mode.** The operator fills it under
  `byo-cert`; the new `deploy/tls-export.sh` fills it under the two ACME modes, reading Caddy's
  certificate *through* the container (so it needs neither root nor Docker's volume layout), refusing
  to guess when the ACME-directory glob is not unique, writing only on a real change, and `SIGHUP`-ing
  whatever `TLS_RELOAD_SERVICES` names. That is the renewal hook ADR-0089's event bus was waiting for.

  **`trusted_proxy_hops` is configuration** (`cloud.toml`, default `1` — today's constant), and
  `client_ip` reads it instead of a hard-coded `TRUSTED_PROXY_HOPS`. `bootstrap.sh` derives it from
  the posture — `2` under `external`, where the chain is `client, terminator, caddy` — and reconciles
  the line on **every** run, because switching a live cell to `external` would otherwise leave it at
  `1` and key the `/admin/login` rate limit on the terminator's single address: every admin in one
  bucket, one person's wrong passwords locking out the rest. `0` is refused at load rather than read
  as caution. Five new unit tests cover both postures, including the one-hop misconfiguration
  directly.

  **A new gate, `cargo xtask tls-modes`**, keeps the accept-list in `bootstrap.sh` and the files in
  `deploy/Caddyfile.d/` in agreement, and requires every posture to import the shared site block.
  `bootstrap.sh` selects a Caddyfile *by name*, so a renamed or missing file would otherwise surface
  on someone's box in the one posture nobody here runs. It does not validate Caddy syntax — that
  needs the binary, and one mode needs a plugin build, which ADR-0090 records as deferred.

  Two fixes that came with it, both in the "supplied configuration is not a generated secret"
  category. **`secrets/caddy.env` is now rewritten when the environment supplies `DOMAIN`** — until
  now changing the `DOMAIN` secret and redeploying did nothing, because the file was kept under the
  never-rotate rule that exists for the database password. And the port publishes are written to a
  generated `deploy/.env`, which Compose reads automatically, so a `docker compose up -d` typed by
  hand keeps the posture instead of silently restoring an internet-facing `:443` on a cell whose
  operator believes TLS terminates upstream.

  **Upgrade note.** No action for an existing deployment: `TLS_MODE` unset means `acme-http01`, and a
  `secrets/caddy.env` written before this change has its posture **inferred once from the old rule and
  recorded**, so a cell already on the Cloudflare path stays on it. Two behaviour changes to know:
  `acme-dns01` with an empty `CF_DNS_API_TOKEN` now **refuses to bootstrap** instead of silently
  downgrading to a challenge that cannot answer for a DNS-only record; and `byo-cert` refuses when the
  certificate files are absent. Add the `tls-export.sh` cron line from
  [`docs/deploy-runbook.md`](docs/deploy-runbook.md) on the ACME modes. Nothing yet alerts on a stale
  export — a deferral flagged in ADR-0090.

- **ADR-0090 — TLS termination is a fork-level posture, chosen explicitly**
  ([ADR-0090](docs/adr/0090-tls-postures.md), roadmap v3 slice **E7a**, debate D24). The decision record
  ahead of the implementation, and the one ADR-0089 was sequenced behind.

  `bootstrap.sh` infers the TLS method from `DOMAIN`'s suffix. Three problems, only one of them about
  certificates. It reaches **two of the four legitimate postures**: a company with its own wildcard from
  an internal CA, and a company whose load balancer or tunnel already terminates TLS, both have to patch
  the repository — which for a framework meant to be forked is the same as unsupported. It **silently
  downgrades**: a managed domain whose `CF_DNS_API_TOKEN` is empty (forgotten, or simply not defined in a
  fork's Environment) falls through to HTTP-01 against a grey-clouded record where nothing inbound on
  `:80` can reach ACME, and the log says it chose HTTP-01 on purpose. And the swap is
  `cp Caddyfile.cloudflare Caddyfile`, which **overwrites a version-controlled file** and leaves the
  posture recorded nowhere — both modes end up in a file called `Caddyfile`.

  The decision: an explicit `TLS_MODE` with exactly four values (`acme-http01`, the default, so an
  existing sslip.io cell is unchanged · `acme-dns01` · `byo-cert` · `external`), each mode's inputs
  checked and **refused loudly** rather than downgraded, each a committed per-mode Caddyfile that
  `bootstrap.sh` installs as a generated file so nothing in the repository is overwritten, the shared
  proxy block imported from one place instead of copied four times, and the chosen mode recorded on the
  box and printed on every run.

  Two consequences beyond the proxy. **`secrets/tls/` becomes the single certificate path in every
  mode**, with each mode saying who populates it — the operator under `byo-cert`, an exporter with a
  loud failure and a renewal hook under the two ACME modes, nobody under `external`. That is the
  dependency ADR-0089 named: TLS on the event bus needs a certificate some posture put there on purpose,
  not a reach into Caddy's private volume layout whose failure mode is a silent expiry two months after
  nobody noticed. And **`trusted_proxy_hops` becomes configuration** (default `1`, today's constant),
  because under `external` the chain is `client, balancer, caddy` and the client is two back — without
  it the login rate limit repaired in this same release keys every request on the balancer's one
  address, so every admin in the company shares one bucket and one person's wrong passwords lock out the
  rest.

  Flagged deferrals rather than silent ones: a stale certificate export is not yet alerted on (an
  exporter that quietly stopped reproduces the same silent expiry, further down the timeline); `caddy
  validate` over all four modes is not in CI, because one mode needs the plugin build; and whether the
  bus can use the exported certificate at all is ADR-0089's implementation to prove on a real box —
  if it cannot, the fallback costs `link-nats` a root-certificate option, which is why that claim is
  named here.

  **Upgrade note.** No action for an existing deployment: `TLS_MODE` defaults to today's behaviour and
  nothing rotates. When the implementation lands, a cell that believed it was on DNS-01 while silently
  running HTTP-01 will **refuse to bootstrap** until either `CF_DNS_API_TOKEN` is set or `TLS_MODE` is
  corrected to match reality.

- **ADR-0089 — the edge reaches the event bus directly, over TLS on its own port**
  ([ADR-0089](docs/adr/0089-edge-event-bus-transport.md), roadmap v3 slice **E7**, debate D23). The
  decision record ahead of the implementation.

  E3 built the outbox publish and wired it into `serve()`; every part of it is live code on a live
  path, and it has never reached a broker. `nats` sits on an `internal: true` network with no published
  port and no proxy route, so `POS_EDGE_NATS_URL` has **no valid value** — and the failure is silent by
  design (`spawn_event_publish` treats an unreachable broker as non-fatal so the store keeps trading),
  which is why it survived. A fleet in that state looks healthy from the till and empty from the cloud.

  Two transports were weighed and they are **performance-indistinguishable** here: the publisher
  batches every 5 s, so ~135 events ≈ 40–100 KB, ~20 KB/s per store. WebSocket's frame header and its
  mandatory client-side masking XOR are a rounding error at that rate. Throughput does not decide it.
  Four things a reverse proxy cannot give do:
  - **Client certificates** — TLS must terminate *at* NATS to authenticate a store by certificate.
    Behind a proxy the client's identity dies at the proxy, and NATS does not read headers. This is
    the one that decided it, because per-store identity on the bus is an already-flagged gap.
  - **Cluster topology** — NATS gossips its cluster and clients failover on their own; through a proxy
    those advertised addresses are unreachable internal ones.
  - **Blast radius** — Caddy runs `cpus: 0.25` / `mem_limit: 96m` and already carries every store's
    config long-poll; the bus would double its persistent connections and make one process the single
    failure for both console and events.
  - **Reconnect cost** — a WebSocket upgrade round-trip per reconnect, which matters for the live mode
    roadmap v3 still defers.

  Two constraints the ADR makes binding. **The port never opens without TLS**: publishing `4222` makes
  the broker internet-facing with no proxy and no firewall in front by default, so its TLS and token
  become the only protection — server TLS and the published port land in the same change, never one
  before the other. And **the client CA must be private** when mTLS follows: a public CA there means
  anyone who can obtain a certificate from it can speak to the bus, which is mTLS configured into a
  no-op. Token authentication ships first; per-store certificates are their own slice, and the CA
  starts on the box as an explicitly-recorded pilot posture that a fleet moves offline before scale.

  Four **revisit triggers** are written down rather than left implicit, because reversing this costs a
  URL and not code (`async_nats::connect` takes either scheme, and `websockets` is already in
  async-nats' default features): mTLS abandoned, a store network that permits only `443`, Caddy
  *measured* as the constraint, or the bus starting to carry large payloads.

  Flagged rather than assumed: whether Docker's `internal: true` permits a published port. It should —
  the flag removes a network's route *out*, and published ports are host→container DNAT — but that is
  not verifiable from a repository, so the implementation slice must prove reachability on a real box
  before the runbook claims it, with attaching `nats` to `frontend` as the fallback.

### Fixed
- **A store provisioned by the wizard can now actually reach its cloud** (roadmap v3, slice **E6**).
  Three defects, one chain:
  - **`config.toml` had no `cloud_url`.** `serve()` composes the cloud surface only
    `if let Some(cloud_url)`, so a box built exactly as the runbook said booted LAN-only: no
    config-pull, no heartbeat, no relay, and `/api/activation` answering 404 so the `/setup` screen
    could not even be reached. The generator now always emits it.

    Worth naming the cause rather than just the fix: the generator's own comment asserted that
    `deny_unknown_fields` *would reject a cloud URL here*. That was true when the wizard was written
    and stopped being true when E1 added the field — so the omission looked deliberate for four
    merged slices. A comment making a claim about another crate's schema goes stale silently, and this
    one is now gone rather than corrected.
  - **`relay_orders` was in no scope picker.** The cloud has enforced it on both `/sync` order routes
    since E3, but neither the wizard nor the API-keys screen offered it, so **no operator could grant
    it from the console** — the relay was unreachable by construction. Both pickers now list it, and
    the wizard pre-selects it beside `read_config`, because a key with only the latter produces a
    store that syncs its configuration and looks healthy while the relay answers `403` on every poll:
    cloud-placed orders never reach the kitchen and the only symptom is a log line.
  - **There was no way to set a non-default listen port** without hand-editing the file. The handoff
    step now offers one, and emits `bind` only when it differs from the edge's `8787`. It sits on the
    handoff step rather than beside the store's name on purpose: the port is a property of *that
    machine*, never of the store, and it never reaches the registry.

  The wizard now produces **two** files: `config.toml` (identity and cloud, no secret) and `env`
  (`POS_EDGE_SYNC_KEY`, plus a commented `POS_EDGE_NATS_URL`) with the install command for
  root-owned mode-0600. They stay separate because only one of them holds a credential, and a support
  screenshot of a config file should never leak one.

  `read_events` and `manage_webhooks` remain deliberately absent from both pickers: they exist in the
  cloud's `Scope` enum but gate no route, so offering them would grant an authority that does nothing.

### Added
- **`docs/fork-checklist.md` — one page for everything a fork configures**
  ([`docs/fork-checklist.md`](docs/fork-checklist.md)). This framework is meant to be forked, and the
  settings a fork must supply were spread across three documents. The new page lists all **12** GitHub
  Actions secrets derived from the workflows themselves (`grep -rno "secrets\.[A-Z_]*"`), marks the
  five that are optional and the one that is conditional, and separates them from the values that are
  **not** repository secrets: the per-store `POS_EDGE_SYNC_KEY` and `POS_EDGE_NATS_URL`, the box's
  `cloud.toml` values, and the Garage keys Garage itself mints.

  It also corrects a real trap: **`RCLONE_REMOTE` was listed in the deploy runbook as a repository
  secret, and no workflow reads it.** It is a box-side environment variable for `deploy/backup.sh`.
  Setting it on GitHub does nothing and leaves the off-box backup tier disabled — a silent no-op in
  the worst possible place. The runbook row is replaced with a note saying so.

### Changed
- **Roadmap v3 corrected against the tree, and three postures settled**
  ([`docs/roadmap-v3.md`](docs/roadmap-v3.md)). An 18-agent audit read every roadmap slice against the
  code rather than the changelog, then tried to *refute* each "done" claim by hunting for the production
  call site. The wiring held up — `serve()` → `compose_cloud_surface` → `spawn_cloud_loops` is a live
  path, verified hop by hop — but the **chain** those slices form has never run end to end. Six findings,
  now a named slice group **A·P1x — Close the chain**:
  - **`/ws` is unauthenticated** (**S0c**). Mounted on the ungated infra router, outside both
    `require_paired_device` and `require_signed_in`, so any host on the store LAN can read the whole
    committed-event fan-out. Previously deferred to B6.1; reclassified as a live hole. B6.1 keeps the
    read-only scope and event filter.
  - **Pairing and sign-in are process-memory only** (**S0d**) — an edge restart re-pairs every device.
  - **Released binaries have no version** (**R1b**) — `version = "0.0.0"` and nothing stamps the tag, so
    the OTA progress model cannot distinguish releases.
  - **A wizard-provisioned store boots LAN-only** (**E6**) — the `config.toml` generator emits no
    `cloud_url`, so activation 404s on a store built exactly as the runbook says; and `relay_orders` is
    absent from the key-issuance UI, so no operator can grant the relay its scope.
  - **The event bus is unreachable** (**E7**) — NATS is on an `internal: true` network with no published
    port, so `POS_EDGE_NATS_URL` has nowhere to point. Silent: the store trades on, the outbox stays
    durable, and the cloud simply receives nothing.
  - **`OtaUpdater` has zero production callers** (**R5**) — the only construction in the tree is a test.
    Four merged slices built update decision, signature verification, self-test and rollback; none of it
    runs.

  **Q1 (the end-to-end acceptance suite) moves up** to run immediately after these, and the Cadence
  section gains a **"name the call site"** rule: every PR must state the line that makes its code
  reachable, because a slice whose only caller is its own test looks identical to a finished one. That has
  now happened seven times.

  Three debates settled from the transport and TLS discussion:
  - **D23** — the edge reaches NATS **directly over TCP**, not through the reverse proxy. Both are
    performance-indistinguishable here (~20 KB/s per store; the publisher batches every 5 s), so the
    decision turns on what a proxy cannot give: TLS terminating *at* NATS is the only way to authenticate
    a store by client certificate, NATS's cluster gossip survives, the proxy is not a shared chokepoint,
    and there is no upgrade round-trip per reconnect. It needs no proxy plugin — the container publishes
    its own port. Four explicit revisit triggers are recorded; reversing costs a URL, not code.
  - **D24** — TLS termination is a **fork-level posture**: ACME HTTP-01, ACME DNS-01, bring-your-own
    certificate files, or termination upstream at the fork's own balancer. An explicit `TLS_MODE` selects
    a snippet instead of overwriting the committed `Caddyfile`. The upstream mode has a hidden edge: the
    app must then trust `X-Forwarded-For`, or the login rate limit collapses every user onto one IP.
  - **D25** — for mTLS, the server certificate and the client CA are **different trust decisions**. The
    client CA must be private; pointing it at a public CA turns mTLS into a no-op. Store certificates
    carry the `store_id` as their subject, which is what finally gives a box a real fleet identity. CA
    custody follows D1's reasoning.

  Milestone estimate rises from ≈70 to **≈80 PR**, 18 merged.

### Added
- **The OTA release registry — the cloud can finally say a release exists** (roadmap v3, slice R2;
  [ADR-0088](docs/adr/0088-ota-artifact-hosting.md)). `pos_cloud::ota` records, for each release tag
  and build target, how big the artifact is and what its SHA-256 digest is, over a `ReleaseStore`
  seam with a `store-postgres` adapter (migration `0038_ota_releases.sql`). Three things worth
  knowing:
  - **The bytes are not in the row.** They live in the object store at
    `releases/{tag}/{target}/pos-edge`, with the detached signature beside them as
    `pos-edge.minisig`, derived from the tag and target by `ota::artifact_key` rather than stored —
    so a row and its blobs cannot disagree about where the bytes are. A release tag is free text
    (`ReleaseTag` deliberately does not validate), so it is checked here, at the point where it stops
    being a label and becomes a path segment: `..`, `/`, and an over-long tag are all refused before
    a key is composed.
  - **A release is immutable.** Re-recording a tag/target with *different* bytes is refused;
    re-recording the identical digest is a no-op, so re-running a release step is not an error. The
    rule lives in one pure function (`ota::admit_artifact`), consulted by the adapter rather than
    duplicated in SQL, so the registry cannot drift from the tests that pin it.
  - **The digest is not a trust boundary.** It catches a truncated upload or a corrupted blob;
    nothing more. Only the detached minisign signature makes an artifact safe to install, and only
    the edge verifies it ([ADR-0047](docs/adr/0047-minisign-verification.md)). The cloud never signs
    and never verifies, which is what makes hosting binaries an acceptable job for it at all.

  `ota_releases` is deliberately the one table in the cloud schema with no `tenant_id` and no
  row-level security: a release is fleet-wide, carries no tenant data, and only the trusted admin
  connection reads it.
- **ADR-0088 corrected against the tree** ([ADR-0088](docs/adr/0088-ota-artifact-hosting.md)). Two
  claims in the merged ADR did not survive contact with the code and are corrected in place rather
  than worked around, because both change what the remaining R2 slices must do. Garage is deployed
  for **backups**, not for media (media renditions are Postgres `bytea` per
  [ADR-0042](docs/adr/0042-image-pipeline.md)), so "no new dependency" was too strong — the
  artifact-storage slice has to add `blob-garage` to `pos-cloud` along with S3 credentials the
  deployment does not yet provision. And the pinned `fetch_update` request
  ([ADR-0054](docs/adr/0054-edge-cloud-http-client.md)) carries only a release tag, while the release
  workflow builds two targets — so the route slice must add an additive `arch` field, which is a wire
  change the ADR said would not be needed. No `PROTOCOL_VERSION` bump either way.
- **Roadmap v3 and the integration doctrine** (roadmap v3, Wave B9.1;
  [ADR-0083](docs/adr/0083-integration-doctrine.md), [`docs/roadmap-v3.md`](docs/roadmap-v3.md)).
  `docs/roadmap-v3.md` lands the approved plan — two parallel programs (Ship the Edge; Complete the
  Domain) across four milestones (v1.0 Ship & Safe → v1.3 Production International) — as the repo's
  durable source of truth. ADR-0083 states the rule the plan's plug-and-play principle rests on: the
  core (`pos-core`, `pos-proto`) holds only the POS invariant, and every external system — a
  marketplace, a card terminal, an e-invoice provider, an external KDS — plugs in through exactly one
  of three points (a port + adapter, the scoped `/v1` API, or the event stream), never as a branch in
  core logic. A new `xtask` check, `vendor-neutral-core`, enforces the automated half: it scans the
  production source of `pos-core` and `pos-proto` (comments and `#[cfg(test)]` code excluded, where
  example data and prose legitimately name vendors) for a curated denylist of vendor brand names and
  fails the build if one appears — the same way the dependency rule (ADR-0013) turned layering into
  law. It runs in `just preflight` and the PR workflow. **Upgrade note:** no wire, permission,
  migration, or dependency change; the core is already vendor-neutral, so the gate ratifies and
  guards the existing state.
- **Signed edge releases** (roadmap v3, slice R1; debate D1,
  [ADR-0047](docs/adr/0047-minisign-verification.md),
  [docs/release-runbook.md](docs/release-runbook.md)). A new `release` workflow cross-compiles the
  `pos-edge` binary for both store architectures (amd64 and arm64), builds the real operator-UI
  bundle into it, signs every artifact with **minisign**, and publishes the artifacts and their
  `.minisig` signatures to a GitHub Release — the same signature the edge verifies before it installs
  an update (ADR-0047). CI builds and signs; the signing key lives only in a GitHub Actions secret and
  never touches a VPS or a store (D1). Cutting a release is gated behind the `production` Environment's
  required reviewer, and the runbook covers generating the keypair and telling the fleet to trust its
  public half. **Upgrade note:** none for the running product — this is release tooling; before the
  first release a maintainer provisions the `MINISIGN_SECRET_KEY` secret (a documented human gate).
  Windows and the OTA artifact server follow in E4 and R2.
- **The edge now dials its cloud: config-pull and heartbeat loops run** (roadmap v3, slice E1;
  [ADR-0085](docs/adr/0085-edge-cloud-sync-transport.md)). The edge already carried the config-pull
  and heartbeat loops as pure, seam-based, fake-tested library code, but `serve()` spawned none of
  them and no real transport existed — so a shipped edge connected to its cloud only at activation and
  then went dark: a menu published from the dashboard never reached the counter, and the fleet view
  showed every store as never-seen. This slice makes them run. A new `cloud_http` module implements
  the two transports over the tree's standard `hyper` + `tokio-rustls` (`ring`) stack — the same pin
  the webhook sender (ADR-0038) and `cloud-sync-http` (ADR-0054) use, already in the lock, so no new
  crate, version, or `cargo-deny` entry — and `serve()` spawns `ConfigClient` and `HeartbeatClient`
  under its graceful-shutdown signal. A published menu/permission/tax/locale change now reaches the
  counter without a restart (config-pull hot-swaps the live session), and a quiet store still reads as
  online (heartbeat). A new optional `cloud_url` field on `EdgeConfig` names the store's cloud; the
  loops authenticate with the tenant-scoped `read_config` API key the `/sync` routes verify today
  (ADR-0037/0039), read from `POS_EDGE_SYNC_KEY` — supplied by the service unit from a mode-0600 env
  file, never committed and never in `config.toml`. The OS-keyring home for the key (E2) and the
  `posdev_` device-credential identity for `/sync` (a later P9 cloud-auth change) stay flagged behind
  the seam. **Upgrade note:** no wire, protocol, `pos-proto`, or migration change. A provisioned store
  gains a `cloud_url` in its `config.toml` and a `POS_EDGE_SYNC_KEY` in its service unit's env file; an
  edge with neither runs LAN-only and spawns no cloud loops, exactly as before. The config-pull loop
  polls on an interval (the cloud does not long-poll yet, ADR-0039), so a publish reaches a store
  within ~30s.
- **The decision for the edge's OS-keyring KeyVault and activation wiring** (roadmap v3, slice E2;
  [ADR-0086](docs/adr/0086-edge-keyvault-and-activation.md)). The edge's activation flow (ADR-0050) —
  type a one-time code, exchange it for a device credential, store it in the OS key store — has been
  written and tested since P9 but never composed into the shipped binary: there is no real `KeyVault`
  adapter (only the in-memory fake), and `activation_router`/`boot_standing` are dangling. ADR-0086
  settles how E2 closes that: a new `key-vault-keyring` adapter over the `keyring` crate (Windows
  Credential Manager, macOS Keychain, and Linux kernel keyutils via `linux-native` — no D-Bus subtree,
  works under the headless service user), contract-suite-verified in the gated integration lane; the
  activation router + a `cloud-sync-http` `CloudSync` composed into `serve()`; a boot gate that gates
  cloud sync but never local trading (offline-first holds); the E1 `read_config` key moved into the
  vault under a new `SecretName::SyncKey` (env override kept); and a `/setup` activation screen in the
  edge UI. Headless-Linux reboot durability (a TPM-sealed credential via `systemd-creds`) stays the
  flagged hardware gate the deferral named. **Upgrade note:** none — this PR records the decision; the
  adapter, composition, and `/setup` screen land in the E2 implementation slices.
- **The OS-keyring `KeyVault` adapter** (roadmap v3, slice E2;
  [ADR-0086](docs/adr/0086-edge-keyvault-and-activation.md)). A new `key-vault-keyring` crate
  implements the `KeyVault` port over the OS credential store via the maintained cross-platform
  `keyring` crate — Windows Credential Manager, macOS Keychain, and on Linux the kernel keyring
  (keyutils) via the `linux-native` backend, which needs no D-Bus session and so works under the
  headless store service user. The socket sits behind a `KeyringBackend` seam, so the adapter's
  mapping and semantics (a `SecretName` to a keyring entry, binary-safe `Secret` round-trip, error
  translation, and every one of the shared `key_vault` contract cases) are proven in the fast
  pull-request gate against an in-memory backend; the real OS store is proven by the same suite behind
  a `--features integration` gate, run on real hardware (a booted box, not a bare CI container, where
  a kernel keyring has no usable session — the hardware handoff ADR-0086 flags). A new
  `SecretName::SyncKey` variant is added for the edge's tenant-scoped `read_config` key (used when the
  composition slice moves it off the environment). This is the first new runtime dependency since the
  HTTP/DB stacks; it passes `cargo deny` (bans, licences, advisories) with no new duplicate-version
  exemption. **Upgrade note:** additive only — a new adapter crate and an additive `#[non_exhaustive]`
  `SecretName` variant; no wire, protocol, or migration change, and nothing composes it into the
  shipped binary yet (that is the next E2 slice).
- **Activation is composed into the shipped edge** (roadmap v3, slice E2;
  [ADR-0086](docs/adr/0086-edge-keyvault-and-activation.md)). The activation flow that has been
  written-but-dark since P9 is now reachable in the real binary: when `cloud_url` is set, `serve()`
  builds the OS-keyring vault (`key-vault-keyring`) and the `cloud-sync-http` `CloudSync`, mounts
  `POST /api/activate` / `GET /api/activation`, and runs the boot gate — a box holding a device
  credential starts the config-pull and heartbeat loops; a fresh box serves the LAN offline and waits
  to be activated (the gate withholds cloud sync, never local trading, ADR-0001). The E1 sync key
  moves to **vault-first**: the loops read the scoped `read_config` key from the keyring
  (`SecretName::SyncKey`) and fall back to `POS_EDGE_SYNC_KEY`, which the service unit now supplies
  from an optional root-owned mode-0600 `/etc/pos-edge/env`. The deploy README and the
  bring-a-store-online guide are updated to match (the old "being composed" status note is gone).
  **Upgrade note:** no wire, protocol, or migration change. A LAN-only edge (no `cloud_url`) behaves
  exactly as before. On a provisioned box, activation now works end to end via `POST /api/activate`;
  the `/setup` operator screen lands in the next slice, and headless-Linux keyring reboot-durability
  remains the flagged hardware gate.
- **A `/setup` screen activates a store server without a terminal** (roadmap v3, slice E2;
  [ADR-0086](docs/adr/0086-edge-keyvault-and-activation.md),
  [ADR-0050](docs/adr/0050-activation-code-exchange.md)). The operator UI now completes the
  activation flow the previous entry composed: the app's boot gate asks the box whether it is
  activated and, if not, lands on **`/setup`**, where the `XXXX-XXXX-XXXX` code from the store
  server's setup sheet is typed into a field that folds Crockford's ambiguous glyphs and groups the
  symbols exactly as the box parses them — so a mistyped code is caught by the checksum on the
  counter rather than after a round-trip. On success the store is activated once and for all and the
  browser moves on to pairing (or straight to the floor if it is already paired); the device
  credential never reaches the browser. A store server with no cloud does not mount the activation
  routes at all, and the screen says so plainly instead of showing a form that cannot work. **Upgrade
  note:** no wire, protocol, permission, or migration change; the new route is additive and the
  activation check never blocks trading — a box that answers "not applicable" or errors carries
  straight on to the counter (ADR-0001).
- **ADR-0087 decides how the store's two outbound rails get connected** (roadmap v3, slice E3;
  [ADR-0087](docs/adr/0087-edge-relay-and-event-publish.md)). Two things the architecture has always
  assumed are still not wired in the shipped binary, and this records how they will be. The **order
  relay** ([ADR-0061](docs/adr/0061-order-relay.md)) has a durable per-store queue in the cloud and a
  tested `RelayClient` and `EdgeOrderIn` at the edge, but no transport and no production caller — so a
  cloud-placed order reaches the queue and stops. **Event publish** has a live cloud consumer and an
  outbox table whose `outbox_batch`/`acknowledge_outbox` have zero production callers — so a committed
  sale never leaves the box. The decision keeps both on rails that already exist: the relay becomes a
  third transport on E1's `CloudHttpClient` (with its own longer timeout, because the client's 15s cut
  under the cloud's 20s long-poll and would have missed every parked order), and the edge publishes its
  outbox over the `MessageLink` port with `link-nats` as the adapter, acknowledging exactly the accepted
  prefix. Both spawn behind E2's boot gate, both are outbound-only, and neither may ever block the
  counter — a cloud that is down grows the outbox and leaves the queue parked while the store keeps
  selling (ADR-0001). Flagged for later, not dropped: escalating backoff when the cloud *refuses* rather
  than fails to answer (both loops share the flaw today), NATS credentials moving into the keyring, and
  carrying the cloud-granted device id forward instead of deriving a system one. **Upgrade note:** no
  wire, protocol, permission, or migration change, and no new third-party dependency (`async-nats` is
  already in the lock via the cloud). One operational consequence to note now: a store key issued with
  `read_config` alone will leave the relay dark — it needs `relay_orders` as well.
- **A cloud-placed order now reaches the kitchen** (roadmap v3, slice E3;
  [ADR-0087](docs/adr/0087-edge-relay-and-event-publish.md),
  [ADR-0061](docs/adr/0061-order-relay.md), [ADR-0064](docs/adr/0064-edge-order-in.md)). The order
  relay is wired into the shipped binary. An activated store with a `cloud_url` now runs a third
  background loop beside config-pull and heartbeat: it long-polls its parked orders from the cloud,
  makes each one through the edge's own intake — repriced against **this store's** menu, deduped on
  the caller's reference inside the order's own transaction, and given a daily queue number that
  survives a restart — and acks the outcome, refusals included, so nothing is silently re-delivered
  for ever. `EdgeOrderIn`, contract-tested since ADR-0064 but never constructed outside a test, gets
  its first production caller; the marketplace / `POST /v1/orders` / QR path runs end to end for the
  first time. The pull carries its own 30-second timeout: the client's ordinary 15 seconds cut under
  the cloud's 20-second long-poll, so a quiet store would have timed out every poll and never seen a
  parked order. Events written by a relayed order carry the box's own stable, store-derived device
  id, because there is no signed-in employee and the order came from the cloud rather than a paired
  till. **Upgrade note:** no wire, protocol, permission, or migration change, and the gate is
  unchanged — no `cloud_url`, not activated, or no sync key still means no relay, and a cloud that is
  down leaves the orders parked while the counter keeps selling (ADR-0001). Two operational notes:
  the store's scoped key must carry **`relay_orders`** as well as `read_config` (without it the edge
  logs a `403` on each pull and the relay stays dark), and `serve()` now takes the queue-number
  authority as a third argument — a source change for anything embedding `pos_edge` directly (the
  real binary passes its SQLite store, the on-fakes example `InMemoryQueueNumbers`).
- **A committed sale now leaves the box** (roadmap v3, slice E3;
  [ADR-0087](docs/adr/0087-edge-relay-and-event-publish.md),
  [ADR-0015](docs/adr/0015-sqlite-access.md)). The store's **outbox** stops being a write-only table.
  Every event the counter commits has always landed there in commit order, but nothing published it:
  the cloud has been running its consumer since P7 with no publisher on the other end, and
  `outbox_batch`/`acknowledge_outbox` had no production callers at all. An activated store that names
  a stream now drains its outbox over the `MessageLink` port: publish a batch, acknowledge exactly the
  prefix the far side accepted, re-send the rest. That prefix rule is the point — the port reports
  acceptance as a count from the start of the batch, so acknowledging the whole batch on a partial
  accept would silently drop the tail, and re-sending the whole batch would make a link that is 60%
  healthy behave like one that is 0% healthy. Delivery is at-least-once by design (there is no
  transaction spanning the stream and SQLite) and the cloud's ingest is already idempotent by event
  id. The stream is configured by a new optional `[nats]` section (`stream` and `subject`, which must
  match the cloud consumer's `stream`/`filter_subject`); the **server URL comes from
  `POS_EDGE_NATS_URL`**, because that is the field where a credential would be embedded and a
  credential never belongs in `config.toml`. **Upgrade note:** no wire, protocol, permission, or
  migration change, and no new third-party dependency — `link-nats` is a workspace crate whose
  `async-nats` subtree the cloud already puts in the lock. Entirely opt-in: with no `[nats]` section,
  no URL, an unactivated box, or an unreachable stream, the edge logs it and publishes nothing while
  the store trades exactly as before and keeps every event (ADR-0001). Configure both, on a store
  whose stream matches the cloud's consumer, and its events start arriving.
- **The till sells the store's own menu** (roadmap v3, slice E5;
  [ADR-0063](docs/adr/0063-store-menu-catalog.md), [ADR-0064](docs/adr/0064-edge-order-in.md)).
  Publishing a menu from the console used to change every channel *except* the till in front of the
  guest: the operator UI carried six pizzas compiled into the app. A new `GET /api/menu` serves the
  store's price book from the same live session an inbound order is repriced against, and the UI
  reads it — `ui/src/lib/menu.ts` is gone. Each item arrives with its price **and** the tax rate
  resolved from the store's own table for the channel a walk-in sale arrives on, so the line the till
  sends carries what the guest was actually shown. An item whose tax class has **no row** in the
  table comes back with no rate and marked unavailable: a missing rate is a configuration error, and
  quietly quoting a guest zero tax is the kind of bug an audit finds rather than a test. The screen
  shows such an item, and an 86'd one, greyed out rather than hidden, so staff can see why it cannot
  be ordered. A store with nothing published says so instead of offering a fictional menu. This also
  takes the first bite out of E5's other half: the app no longer builds any `Money` of its own on this
  path — it hands the edge's own amounts straight back. **Upgrade note:** no wire, protocol,
  permission, or migration change; the route is additive and unauthenticated callers still hit the
  device-token gate like every other domain route. A store that has not published a menu now shows an
  empty menu on the till, where it previously showed six hardcoded items — publish the store's
  catalogue from the console to fill it.
- **The till stops doing money arithmetic** (roadmap v3, slice E5;
  [ADR-0028](docs/adr/0028-settlement-and-payment-invariant.md)). The operator UI computed the running check
  itself — summing the lines and applying a tax rate **hardcoded at 10%** — so a store on any other
  rate, or with more than one tax class, showed the guest one number and settled against another. A
  new `GET /api/tables/{id}/check` answers with the edge's own figure, assembled by the same
  `billing::assemble` the settle path runs, over the same projection and the same session: one
  calculation, living in the domain. The pay screen now displays that figure rather than deriving
  one, and takes the store's currency from it instead of assuming `VND`, so a store on another
  currency renders and tenders correctly with no change here. An acceptance test proves the point
  end to end — the check the till reads settles the bill exactly, and a settle for any other amount
  is refused. A table with nothing on it reads as owing zero rather than erroring. **Upgrade note:**
  no wire, protocol, permission, or migration change; the route is additive and behind the same
  device-token gate as every other domain route. Change due is still shown as the tender minus the
  edge's total while the operator taps — a subtraction of two figures they can see — with the
  authoritative `change_given` recorded per payment at settle. The shift screen's opening float still
  assumes `VND`; that is the one place left and it is its own slice.
- **ADR-0088 decides how the cloud hosts an update artifact** (roadmap v3, slice R2;
  [ADR-0088](docs/adr/0088-ota-artifact-hosting.md)). The over-the-air path is built at both ends and
  joined in the middle by nothing: the edge decides against the published rollout, fetches, verifies
  the minisign signature, installs, self-tests and reports — and the route it fetches from,
  `POST /internal/ota/artifact`, **does not exist** in the cloud. A store that decides it should
  update gets a 404. The decision: store artifacts in Garage over the `BlobStore` port already in the
  tree (no new dependency, no new infrastructure), serve them at the route the edge's adapter already
  calls (so no wire change and no `PROTOCOL_VERSION` bump), authenticate with the store key the box
  already holds, and keep the cloud a **dumb host** — it never signs and never verifies, because the
  edge must verify anyway or the cloud becomes a trust boundary; a compromised cloud can make an
  update fail, never make a box install code. Uploading and promoting are audited `/admin` actions,
  and a release is immutable once uploaded, because a version that changes under a fleet is not a
  version. Rejected explicitly: pointing stores at GitHub Releases (it puts a third party on the path
  that installs code, and leaks the fleet's update cadence), signing at the cloud (keys never touch a
  VPS — debate D1), and 30 MB blobs in Postgres. Flagged for later: streaming rather than buffering
  the response, the exact scope the route requires, artifact garbage collection, and the egress a
  fleet-wide ring implies. **Upgrade note:** documentation only — no runtime change.

### Changed
- **The contract and soak CI jobs now run for real, and Dependabot is on** (roadmap v3, slice C1).
  The `contract` job in `main.yml` was a placeholder `echo`; it now runs `cargo test --workspace
  --test contract`, so every adapter's port contract suite (the `pos-contract-tests` harness) runs on
  each push — "swappable" is a checked fact, not a claim. The nightly `soak` job likewise ran nothing;
  it now runs the `pos-simulator` scenario tests (which assert the published capacity-and-reliability
  envelope) and prints the report, so a modelled-throughput regression fails the build. A new
  `.github/dependabot.yml` opens weekly, grouped, capped dependency PRs for the Cargo workspace, the
  GitHub Actions pins, and both pnpm front-ends (`ui`, `dashboard`) — each landing on the same `pr.yml`
  gate (`cargo deny`, tests, the pinned-SHA check) so a bad bump cannot merge. **Upgrade note:** none —
  CI and tooling only; no code, schema, protocol, or permission change.

### Security
- **The edge now requires a paired device token on every domain route** (roadmap v3, slice S0;
  [ADR-0084](docs/adr/0084-device-authentication.md), amending
  [ADR-0030](docs/adr/0030-pairing-and-offline-auth.md)). Until now the edge issued a device token
  when a tablet paired but never checked it: every order, bill, and shift route resolved a fixed
  `dev_actor()` (employee 1 / device 1) and accepted any caller on the store LAN, so any host on the
  store network could seat tables, settle bills, close shifts, and read the floor. Pairing now binds
  each issued token to a `DeviceId`, and a `require_paired_device` middleware validates the
  `Authorization: Bearer <token>` on the whole domain router — reads and writes alike — answering a
  generic `401` for an absent, malformed, or unknown token, and stamping the resolved device onto the
  command's actor. The operator UI stores the token it receives on pairing, sends it on every call,
  routes an unpaired device to the pairing screen, and drops a stale token on a `401` to re-pair.
  **Deliberately scoped:** this authenticates the *device*; the acting *employee* stays a placeholder
  until PIN sign-in (a following slice), `/ws` read-auth lands with its own slice, and the issued
  token set is in memory (an edge restart re-pairs). **Upgrade note:** a device must pair before it
  can use the edge; no cloud protocol, migration, or `pos-proto` change — the `POST /api/pair` request
  and response shapes are unchanged, only presenting the token they carry is now required.
- **A member of staff must sign in on a paired device before it can act** (roadmap v3, slice S0b;
  [ADR-0084](docs/adr/0084-device-authentication.md) amendment). S0a authenticated the *device* but
  every command still ran as a placeholder employee. Now a paired device commands nothing until
  someone signs in with their badge code and PIN: `POST /api/session/sign-in` verifies the PIN offline
  against the synced roster through the existing five-fail/five-minute lockout (ADR-0030) and binds the
  device to that employee, so every sale, void, and shift is attributable to who did it; `POST
  /api/session/sign-out` and `GET /api/session` complete the flow. A `require_signed_in` middleware
  resolves the signed-in employee into each command's actor, and a paired device with nobody signed in
  is refused `403` (distinct from the unpaired `401`) — the placeholder `device_actor` is deleted. The
  edge reads the employee id the `permissions` node already publishes (ADR-0070); an unknown code and a
  wrong PIN answer identically, so a probe cannot enumerate staff codes. The operator UI adds a sign-in
  screen after pairing and a sign-out control on the status bar. **Deliberately scoped:** the acting
  employee is now real, but *what* they may do still reads the store default grant, not the person's
  own permission set (per-actor enforcement lands with the capability pass); the sign-in binding is
  in-memory (an edge restart re-signs-in) and logs only the employee id and outcome, never the PIN.
  **Upgrade note:** a paired device must now sign a staff member in before any command; no cloud
  protocol, migration, or `pos-proto` change — the edge reads a field the published `permissions` node
  already carries.
- **A console admin can re-enrol their authenticator and hold one-time recovery codes** (roadmap v2,
  Track G, G1 slice 6; [ADR-0067](docs/adr/0067-multi-admin-console-rbac.md)). `POST /admin/totp`
  rotates the TOTP secret the sign-in verifies — after re-confirming the current password, so a
  session-only attacker (holding the cookie but not the password) cannot lock the owner out — and
  returns a fresh one-time enrolment (provisioning QR + base32 secret); existing sessions stay valid
  and the next sign-in uses the new authenticator. `POST /admin/recovery-codes` (re)generates ten
  one-time codes, shown once and stored only as their `SHA-256`, and `GET /admin/recovery-codes`
  reports how many are left (never the codes). `/admin/login` now accepts a `recovery_code` in place
  of the TOTP code when the authenticator is lost: the password is verified first (a wrong password
  never burns a code), the code is consumed atomically single-use, and every failure collapses to the
  same generic `401` — the recovery path is no more of an oracle than the TOTP one. Both management
  routes are self-service (any authenticated admin, no role permission). Because sign-in is still the
  single super-admin credential (per-admin email login is a later slice), re-enrolment and recovery
  act on that credential and the codes belong to the owner. **Upgrade note:** the `admin_recovery_codes`
  table already exists (migration `0018`); no new migration, protocol, or permission change; the
  `LoginRequest` gains an optional `recovery_code` field (absent for an ordinary sign-in).
- **The admin sign-in is rate-limited, and the console now ships defence-in-depth response headers**
  (roadmap v2, Track G, G1 slice 5; [ADR-0067](docs/adr/0067-multi-admin-console-rbac.md)).
  `/admin/login` throttles attempts in a sliding window — by default 10 per 5 minutes per client
  (`admin_login_max_attempts`, `admin_login_window_secs`), keyed by client IP today and ready for a
  per-email key when email login lands — refusing the excess with `429 Too Many Requests` and a
  `Retry-After`. The check runs **before** the Argon2id verify, so an online guesser costs a cheap
  refusal rather than a hashing storm, and because it precedes the credential check it can never
  become an oracle for whether a password was right; a refused attempt is not recorded, so it does
  not push a legitimate admin's next try further out. Every console response now carries
  `Content-Security-Policy` (scripts locked to `'self'` — the built SPA has no inline script — with a
  bounded `'unsafe-inline'` for styles only), `X-Content-Type-Options: nosniff`,
  `X-Frame-Options: DENY` (plus the CSP's `frame-ancestors 'none'`) against clickjacking, and
  `Referrer-Policy: no-referrer`. **Upgrade note:** no schema, protocol, or permission change; two new
  optional config values with safe defaults. The rate-limit state is in-process (the cloud is a
  single box) and ephemeral — a restart clears it, failing open rather than locking anyone out. The
  CSP is deliberately strict on scripts; if a future console feature needs a new subresource origin,
  widen `CONTENT_SECURITY_POLICY_VALUE` in `crates/pos-cloud/src/http.rs` rather than loosening
  `script-src`.

### Fixed
- **The media isolation integration test no longer trips the global `media_id` primary key** (M5
  follow-up; test-only). The merge-time `store-postgres` integration job went red after the M5 media
  rail merged: `media::stores_reads_one_rendition_lists_and_stays_tenant_scoped` inserted the same
  `media_id` for two tenants to probe isolation, but `media_assets.media_id` is a global `text`
  primary key (migration `0030`), so the second insert violated the key and the adapter surfaced it as
  `PortError::Unavailable`. It stayed hidden on the pull-request suite because the Postgres integration
  tests run only on merge (behind the `integration` feature). Media ids are globally-unique ULIDs in
  production, so the shared-id premise never occurs; the neighbour tenant now owns a distinct asset and
  the test asserts the real guarantee — a tenant reading another tenant's asset by id gets `None`, each
  tenant reads only its own, and delete/listing stay tenant-scoped. `PostgresMedia` already filters
  every read by `tenant_id`, so no adapter change was needed. **Upgrade note:** none — test-only; no
  adapter, schema, or migration change.
- **Admin sign-in works on a real PostgreSQL deployment again** (Track G, G1 slice-4 regression
  caught by the merge-time integration job). The rewritten `insert_session` wrote `created_at` with
  `to_timestamp($2::double precision / 1000.0)`, which makes PostgreSQL infer the `$2` parameter as
  `double precision`; the adapter binds it from a Rust `i64` (`bigint`), so the extended-protocol type
  check rejected every session insert against real PostgreSQL — and since a successful login *creates*
  a session, admin sign-in would have failed on any real deployment. The in-memory fake used by the
  unit tests does not type-check parameters, so the pull-request suite stayed green; the
  `store-postgres` integration job (real PostgreSQL, run on merge) is what surfaced it. The cast is now
  `$2::bigint`, matching the bind, and the three admin-session integration tests pass against
  PostgreSQL 16. **Upgrade note:** none — a one-line SQL fix; no schema, protocol, or permission change.
- **The cloud catalog tables are now actually created on deployment** (Track G groundwork). Migrations
  `0013`–`0017` (catalog tax classes, item taxonomy, display/layout, modifier groups, menu sections)
  were added to the `store-postgres` migrations directory during Phase 2a but never embedded into the
  boot-time runner, which stopped at `0012` — so on a real PostgreSQL deployment the catalog tables
  were never created. The runner now applies `0013`–`0018` idempotently after `0012`. All are
  forward-only, additive `CREATE TABLE IF NOT EXISTS` migrations, so an installation that somehow
  already had the tables is unaffected. The gap was hidden because the `store-postgres` integration
  tests run only on merge (behind the `integration` feature) and the migration drift-gate watches only
  the edge (`store-sqlite`) migrations. **Upgrade note:** additive migrations only; no rollback risk.
- **The admin console no longer surfaces `tenant_id … is not a ULID`, and every scoped screen loads
  its data on open** (Track F, F0). The `/admin` screens guarded the working context inconsistently —
  several fired their first request with an empty tenant/store id and surfaced the raw backend
  `… is not a ULID` `400`, and every screen sat blank behind a manual "Load" button. A shared context
  contract (`dashboard/src/lib/scoped.tsx`) replaces both: `RequireContext` renders a screen — and
  lets it fetch — only once its tenant, or tenant *and* store, is chosen, showing a "pick it in the
  top bar" panel otherwise; `onScopedContext` loads on mount and again whenever the context changes,
  but never with an empty id. Every scoped screen (Reports, Stores, Catalog, Layout, Config, API keys,
  Devices, Webhooks, Translations, Activation) now uses it, and the manual Load button became a
  Refresh. A `401` from any call now drops the client's authed flag so the shell returns the operator
  to the login screen instead of stranding them on a view that can no longer load, and the guided
  new-store wizard mints its store exactly once even if the operator steps back and forward.
  **Upgrade note:** none — operator-UI behaviour only; no schema, protocol, or permission change.
- **The "success" green now clears WCAG-AA contrast as text in the light theme** (P6 exit criterion,
  #44). A numeric audit of the design-token palettes (`docs/wcag-contrast-audit.md`) measured every
  colour pair the interface renders, in both themes, and found the light-theme `--ok` token at 3.72:1
  on `surface` — below the 4.5:1 needed for normal text, and `ok` is used as small text (the fired-line
  badge, the shift-variance line, the paired/settled confirmations). It was darkened from
  `oklch(0.6 …)` to `oklch(0.52 …)`, giving 5.16:1. Every other text pair already passed. A new
  `pnpm contrast` gate parses `tokens.css` and fails the build if any text pair drops below AA; it runs
  in both the `ui` and `dashboard` builds and CI. The remaining sub-3:1 tokens (the 1px separator and
  the table-state dots) are non-text and exempt — the dots are `aria-hidden` and always ride with a
  text label, so meaning never depends on the hue. **Upgrade note:** a default value moved — the light
  `--ok` token is darker; the dark theme is unchanged.
- **Super-admin sign-in after enrolment now works with any authenticator app** (ADR-0034 amended).
  The mandatory TOTP second factor ran over HMAC-**SHA256**, but Google Authenticator and Microsoft
  Authenticator ignore the `otpauth://` URI's `algorithm` field and always compute **SHA1** — so the
  6-digit codes never matched and `/admin/login` rejected every attempt with the generic "code or
  password not accepted" right after `/admin/setup`. TOTP now runs over **HMAC-SHA1** (the RFC 6238
  default that every app supports), and the provisioning URI states `algorithm=SHA1`. The 20-byte RFC
  6238 SHA1 test vectors and the URI assertion prove it. **Upgrade note:** only the server's TOTP
  HMAC changes (SHA256 → SHA1); the stored secret is untouched. An operator who used Google or
  Microsoft Authenticator — the apps whose SHA1-only behaviour caused the failure — can simply sign
  in once this deploys, because their existing entry already computes SHA1 and now matches. An
  operator whose app *did* honour SHA256 (Aegis, 1Password, FreeOTP) must re-enrol via
  `deploy/reset-admin.sh` (the `reset_admin` break-glass, ADR-0045), since the one-time secret cannot
  be re-exported. There is at most one super-admin.

### Added
- **The Layout screen becomes a visual per-channel editor** (roadmap v2, Track F3, F3 slice 4;
  [ADR-0082](docs/adr/0082-catalog-and-layout-rebuild.md)). The 710-line hand-typed layout editor is
  rebuilt on the F2 kit: per channel, positioned buttons render on a **device-shaped grid preview** at
  their `(column, row)` (click a tile to edit it), a client-side **collision check** flags — and
  stacks, never hides — two buttons sharing a cell, **copy-between-channels** clones a channel's
  buttons to another (behind a confirm), and the flowing buttons (no grid slot) reorder through the
  kit's `ReorderList` — F3 is its first consumer, which now gains **pointer drag layered over its
  keyboard up/down** (the arrows stay the accessible fallback) — writing each button's order. The button editor and
  the display-taxonomy (display categories + sub-categories) move into kit `DataTable`s, a `Drawer`,
  and a `ConfirmDialog`, and the `channel`/`StatusCell`/error helpers are now shared with the Catalog
  sub-screens. **Upgrade note:** none — frontend-only; the `layout` config node, the compile/publish
  path, and the backend contract are unchanged; moving a button still reprices nothing.
- **The Menu screen is now a tabbed shell over its five kit sub-screens; the 1,898-line monolith is
  gone** (roadmap v2, Track F3, F3 slice 3; [ADR-0082](docs/adr/0082-catalog-and-layout-rebuild.md)).
  `/catalog` now renders `CatalogShell` — one route, one nav entry, one breadcrumb, with a tab bar
  across Items, Taxonomy, Tax classes, Modifiers, and Menus. The shell owns the page header and the
  tenant `RequireContext` gate, and mounts only the active tab, so each sub-screen's auto-load fires
  on first view and the five never fetch at once. `screens/Catalog.tsx` is deleted. **Upgrade note:**
  none — the `/catalog` URL, nav entry, and backend contract are unchanged; only the screen's
  internals changed.
- **The Menus sub-screen lands on the kit with a currency-aware money field and bulk price editing**
  (roadmap v2, Track F3, F3 slice 3; [ADR-0082](docs/adr/0082-catalog-and-layout-rebuild.md)). The
  priced heart of the catalog — menus (with inheritance), authoring sections, per-channel placements,
  and publish-to-store — becomes `screens/catalog/Menus.tsx` on the kit: menus, sections, and
  placements are each a `DataTable`, and create/edit move into `Drawer`s. Two F3 additions ride along:
  a new **`MoneyField`** (in `components/ui.tsx`) edits a price as an integer in a currency's smallest
  unit (the exact `amount_minor` stored), grouped for the active locale as the operator types, with
  the currency now **chosen from the country registry** (`GET /admin/countries`) instead of a free-text
  box; and a **bulk price editor** sets one channel's price across a section's placements (or the whole
  menu) at once — or clears it — over the same audited `setPlacement` path, leaving the other channels
  untouched. Removing a placement now runs through a `ConfirmDialog`. Built unrouted alongside the
  monolith; the shell and route swap follow. **Upgrade note:** none — frontend-only, no `/admin`
  route, permission, or migration change; prices remain **T2**, authored in the console and shipped in
  config, never logged.
- **The Menu screen's Items, Tax classes, Taxonomy, and Modifiers move onto the F2 CRUD kit**
  (roadmap v2, Track F3, F3 slice 2; [ADR-0082](docs/adr/0082-catalog-and-layout-rebuild.md)). The
  first four entities split out of the 1,898-line Catalog monolith: `screens/catalog/Items.tsx`,
  `screens/catalog/TaxClasses.tsx`, `screens/catalog/Taxonomy.tsx` (item categories +
  sub-categories), and `screens/catalog/Modifiers.tsx` (modifier groups), each on a small shared
  `screens/catalog/shared.tsx`. Every list is now a searchable, sortable, paginated `DataTable` with
  the entity's ULID tucked behind a `TechnicalDetails` disclosure and its status shown as a
  `StatusBadge`; create and edit move into a `Drawer` in place of the monolith's inline table
  editing. Behaviour is preserved throughout: an item takes a name, a required tax class, and
  optional category/sub-category at create, and edit changes its name and per-locale names
  ([ADR-0074](docs/adr/0074-localization-and-tax.md)) while re-sending its taxonomy untouched; the
  inline image widget ([ADR-0075](docs/adr/0075-media-and-file-rail.md)) and the owner/admin CSV
  export ride along; a sub-category is created under a required parent, kept on rename; a modifier
  group is authored with its min/max rule, member items, and attachments, then renamed or
  archived/restored with the rule and members re-shipped unchanged; and archive/restore stays a
  one-click reversible toggle everywhere. Built alongside the existing `Catalog` screen (not yet
  routed), so the monolith stays live and the build stays green until the shell lands in a later
  slice. **Upgrade note:** none — frontend-only, no `/admin` route, permission, or migration change.
- **Catalog & Layout rebuild gets an architecture** (roadmap v2, Track F3;
  [ADR-0082](docs/adr/0082-catalog-and-layout-rebuild.md)). Frames the last Track-F work: the two
  remaining pre-F2 dashboard screens are rebuilt on the F2 CRUD kit. The 1,898-line Catalog monolith
  becomes five tabbed, searchable sub-screens under one `/catalog` route — Items, Taxonomy, Tax
  classes, Modifiers, and Menus — each on `DataTable`/`Drawer`/`ConfirmDialog`/`StatusBadge`/
  `TechnicalDetails`; a new `MoneyField` gives currency-aware price fields (locale-grouped, storing
  minor units) and the Menus sub-screen gains bulk price editing. Layout becomes a visual per-channel
  grid with a device-shaped preview, client-side collision detection, copy-between-channels, and
  drag-to-reorder (the kit's `ReorderList` gains pointer drag over its keyboard fallback). Behaviour-
  preserving and frontend-only — the Phase 2a backend and the ADR-0066 authoring model are untouched.
  Deliberately deferred and flagged: reading the store's published currency to prefill the price field
  (no such read exists yet), full free-form grid pointer-drag polish, and the Fork-A shared-web-kit
  extraction. **Upgrade note:** none — an ADR; the sub-screens and Layout rebuild land in the following
  slices, all frontend and additive.
- **Reports & analytics gets an architecture: windowed rollups, revenue, and X/Z close semantics**
  (roadmap v2, Track O4; [ADR-0081](docs/adr/0081-reports-and-analytics.md)). Frames the reporting
  track additively over the one materialised-rollup projector (ADR-0036): date-range/windowed reads
  so the API stops shipping a store's entire history; a registry-driven projector that reads the
  Active-store list instead of scanning `SELECT DISTINCT … FROM events` every pass (perf wave 2);
  revenue and product-mix rollups decoded from the settlement and line-fired events the projector
  already ingests, gated behind a new `console.reports.revenue` permission because prices are **T2**;
  and a resolution of the long-open **D10** spec gap — an X report is a non-resetting on-demand read of
  the current period, a Z report is a one-time immutable per-business-date close artefact served
  verbatim thereafter. Deliberately out of scope and flagged: per-employee performance analytics (an
  employee-monitoring boundary the org and the metrics port both draw), the scale-only perf reshapes
  (dirty-marking, windowed storage blobs, config-blob delta, request-latency histograms), and the
  country-specific statutory report layouts. **Upgrade note:** none — an ADR; the routes, permission,
  and projector changes land in the following slices, all additive.
- **Rollup reads take a date-range window, and no longer ship a store's whole history** (roadmap v2,
  Track O4; [ADR-0081](docs/adr/0081-reports-and-analytics.md)). Both daily-rollup routes —
  `GET /v1/stores/{store_id}/rollups/daily` and `GET /admin/stores/{store_id}/rollups/daily` — accept
  `?from=&to=&limit=`: an inclusive `YYYY-MM-DD` business-date range and a cap on the days returned
  (the newest kept, still oldest-first). A malformed date, `from` after `to`, or a zero `limit` is a
  `400`; `limit` is clamped to a year. When a read names no window it now returns the **most recent 90
  trading days** rather than every retained day. The dashboard's `dailyRollups` client takes an
  optional window and is otherwise unchanged. **Upgrade note:** a rollups read with no query params
  now returns at most the last 90 trading days instead of the full history — pass an explicit
  `from`/`to`/`limit` (up to 366 days) to widen it. Counts only; no PII, no protocol/migration change.
- **The rollup projector reads the registry instead of scanning the event log** (roadmap v2, Track O4
  "perf wave 2"; [ADR-0081](docs/adr/0081-reports-and-analytics.md)). The background projector decided
  which stores to fold from `SELECT DISTINCT tenant_id, store_id FROM events` — a full scan of the
  event table on every 30-second pass. It now lists the registry's **Active** stores (ADR-0065), a
  metadata read. "The fleet" the projector maintains is now the provisioned, active stores: a store is
  registered at provisioning, so an event-bearing store is registered; an archived store drops out of
  the sweep (its stored rollup is left intact and still readable), and a registered store with no
  events folds nothing. **Upgrade note:** none for the API — the rollup contents and read routes are
  unchanged; the only difference is which stores the projector visits. No protocol, migration, or
  permission change.
- **Revenue and product-mix rollups, behind a new revenue permission** (roadmap v2, Track O4;
  [ADR-0081](docs/adr/0081-reports-and-analytics.md)). The projector now decodes the money-bearing
  events it already ingests — `billing.bill.settled` (subtotal, reductions, service charge, tax,
  total_due) and `sales.order_line.added` (per-item ordered quantity and value) — into a parallel
  per-trading-day `DailyRevenue` rollup, served windowed at `GET /admin/stores/{store_id}/revenue/daily`.
  Because prices are **T2**, the route and the rollup are gated behind a new **`console.reports.revenue`**
  permission (Owner/Admin only — Ops and Viewer are refused), narrower than the counts read's
  `console.data.read`. Revenue is recognised from settlement; the product mix is the **gross ordered**
  mix (before voids/comps, which the line events do not carry back to a menu item), so it reads menu
  popularity, not per-item recognised revenue. No customer or employee identifier enters the rollup.
  **Upgrade note:** a new permission `console.reports.revenue` (granted to Owner and Admin by default);
  no migration (the revenue rollup rides the existing `rollups` blob under a `#[serde(default)]` field)
  and no protocol change. Revenue accrues from the projector's cursor forward; reset a store's rollup
  (the ADR-0036 lever) to backfill its history.
- **X and Z reports, resolving the long-open D10 spec gap** (roadmap v2, Track O4;
  [ADR-0081](docs/adr/0081-reports-and-analytics.md)). `GET /admin/stores/{store_id}/reports/xz`
  returns a store trading day's report bundling its activity counts, revenue, and a new cash-drawer
  summary (opening float, paid-in/out, shifts opened/closed, and the blind-close expected/counted/
  variance totals, folded from the shift and drawer events). The **D10** questions are settled: an
  **X** report is the current (open) day's running totals — non-resetting, recomputed each call; a
  **Z** report is a **closed** day's totals — a past day that no longer receives events, so the same
  read returns the same figures verbatim thereafter. `kind` is `"X"` for the latest day present and
  `"Z"` for any earlier day; omit `business_date` for the current-day X. It is the operational
  daily-close record, **not** the country module's legal invoice artefact. T2 (it exposes money), so
  gated behind `console.reports.revenue`. Dashboard `XzReport`/`DailyCash` types + `xzReport` client.
  **Upgrade note:** none beyond the `console.reports.revenue` permission already introduced with the
  revenue rollup; the cash summary rides the same `#[serde(default)]` rollup blob (no migration).
- **Reports export to CSV** (roadmap v2, Track O4; [ADR-0081](docs/adr/0081-reports-and-analytics.md),
  reusing the ADR-0075 rail). `GET /admin/stores/{store_id}/rollups/export` streams the windowed daily
  activity counts as a CSV (behind `console.data.read`), and `GET
  /admin/stores/{store_id}/revenue/export` streams the windowed daily revenue totals (behind
  `console.reports.revenue`, because prices are **T2**). Both accept the same `?from=&to=&limit=`
  window as the reads, set `content-disposition: attachment`, and are audited by **row count only** —
  never the contents (`reports.export_rollups` / `reports.export_revenue`). **Upgrade note:** none —
  additive read/export routes reusing existing permissions; no protocol or migration change.
- **The Reports screen is rebuilt into a real analytics view** (roadmap v2, Track O4;
  [ADR-0081](docs/adr/0081-reports-and-analytics.md)). The console's home screen gains a date-range
  window (defaulting to the last 90 trading days), inline-SVG trend charts (no chart library — nothing
  past the CSP to load), and a CSV export button on each section. For Owner/Admin (revenue is **T2**,
  so the panels are hidden for Ops/Viewer, matching the server's `console.reports.revenue` gate) it
  adds a revenue table and trend, a product-mix top-10 (by ordered value across the window), an X/Z
  report card (today's X or a chosen closed day's Z, with the cash-drawer expected/counted/variance),
  and a cross-store comparison of revenue across the tenant's active stores. Counts stay visible to
  every role. Full en/vi. **Upgrade note:** none — a console screen over the O4 read/export routes; no
  protocol, migration, or permission change.
- **The console gets a Channels & payments screen to author how a store sells** (roadmap v2,
  Track M7; [ADR-0080](docs/adr/0080-channels-and-payments.md)). A new Channels & payments screen
  (under Master data, Owner/Admin) drives the four M7 nodes for one store without touching JSON: the
  sales channels it accepts, the payment methods it takes as tender, its QR ordering guardrails
  (enabled, staff-confirmation, per-table limit, rate window, optional business hours), and its
  per-marketplace vendor policies (add/remove rows of vendor, availability, prep minutes, enabled).
  Each card publishes its own node and is store-scoped — an unpublished section stays never-blank, so
  before publishing every channel and tender shows enabled and the QR defaults show through. Full
  en/vi translations, and it reuses the existing `console.config.publish` permission. **Upgrade note:**
  none — a console screen over the M7 publish routes; no protocol, migration, or permission change.
- **Marketplace vendor policies can be authored per store** (roadmap v2, Track M7;
  [ADR-0080](docs/adr/0080-channels-and-payments.md)). New `GET`/`PUT /admin/config/vendors` routes
  read and publish a store's per-marketplace policies as the `vendors` config node: per vendor, whether
  it is enabled, its availability (open/busy/closed — a new `VendorAvailability` wire enum mirroring the
  `DeliveryVendor` busy-mode), the prep time authored for the busy case, and the menu items suppressed
  (86'd) on that vendor. Behind `console.config.publish`, audited; a policy naming an unknown
  availability is refused. This is the authoring surface only — the live loop that pushes busy-mode/86
  to a marketplace from the policy is the flagged follow-up (same shape as the campaign live-eval and
  inventory marketplace-notify deferrals). **Upgrade note:** none — additive `pos-proto` types (incl. a
  new closed-vocabulary enum) and settings routes; no protocol, migration, or permission change.
- **QR ordering guardrails are authorable, and the edge finally honours staff-confirmation** (roadmap
  v2, Track M7; [ADR-0080](docs/adr/0080-channels-and-payments.md)). New `GET`/`PUT /admin/config/qr`
  routes read and publish a store's QR guardrail settings — enabled, staff-confirmation-required,
  per-table rate limit, rate window, and business hours — as the `qr` config node the cloud's QR
  intake already reads (behind `console.config.publish`, audited; hours validated to `0..=23`). The
  node existed and the cloud read it, but the edge ignored `staff_confirmation_required` and held every
  table-bearing QR order for staff unconditionally; the edge now reads the node so an operator can turn
  the hold off. Default stays on (ADR-0057), and an absent node leaves the running value untouched.
  **Upgrade note:** none — additive route and edge read; no protocol, migration, or permission change.
- **A store can now be told which channels and tenders it accepts, and the edge enforces it** (roadmap
  v2, Track M7; [ADR-0080](docs/adr/0080-channels-and-payments.md)). New `GET`/`PUT
  /admin/config/channels` and `.../tender` routes read and publish a store's enabled sales channels
  and accepted payment methods as the `channels` and `tender` config nodes (behind
  `console.config.publish`, audited; authoring rejects an unrecognised token up front). The edge
  applies both in `session_from_config`: order intake refuses a sales channel the store does not list,
  and bill settlement refuses a payment method it does not accept. Opt-in and never-blank — a store
  that has published neither node trades on every channel and takes any known tender, exactly as before
  M7. **Upgrade note:** none — additive settings routes and edge gates; no protocol, migration, or
  permission change, and an absent node imposes no restriction.
- **Channels and tender get authorable node types, so a store can be told which it accepts** (roadmap
  v2, Track M7; [ADR-0080](docs/adr/0080-channels-and-payments.md)). Today a sales channel is enabled
  only implicitly (by the menu carrying a row for it) and the accepted payment methods are fixed in the
  edge. This slice adds the wire shape for authoring them: `pos_proto::channels::PublishedChannels`
  (the enabled `SalesChannel` set) and `PublishedTender` (the accepted `PaymentMethod` set), each a
  list of forward-compatible `Open` tokens; and `pos_core::channels::{enabled_channels,
  accepted_tender}` build the domain sets the edge will enforce, dropping any unspecified/unrecognised
  token rather than silently switching it on. Opt-in and never-blank: an absent node means no
  restriction (exactly today's behaviour), a present node is authoritative. The publish routes, edge
  gates, and console follow in the same track. **Upgrade note:** none — additive `pos-proto`/`pos-core`
  modules; no protocol, migration, or permission change, and a store with no channels/tender node
  behaves exactly as today.
- **A goods receipt can name the supplier it came from** (roadmap v2, Track M6;
  [ADR-0079](docs/adr/0079-inventory-and-suppliers.md)). The `inventory.stock.received` event gains an
  optional `supplier_id` field — a reference to a supplier authored in the `inventory` node — so a
  goods receipt records who it came from. Additive and unversioned: a receipt that names no supplier,
  or a producer that predates the field, decodes to `None`; it is a reference only, with the full
  purchasing relationship staying in the ERP. The goods-in/stocktake operator flow that emits a receipt
  remains the flagged edge follow-up. This completes Track M6 (inventory & suppliers): the finished §8
  engine now has cloud authoring, a published node, an edge that applies it, and a console to drive it.
  **Upgrade note:** none — one additive event field; no protocol-version bump.
- **The console gets an Inventory screen to author recipes without touching JSON** (roadmap v2,
  Track M6; [ADR-0079](docs/adr/0079-inventory-and-suppliers.md)). A new Inventory screen (under
  Master data, Owner/Admin) lists and edits a tenant's ingredients (name + unit), recipes, and
  suppliers on the F2 CRUD kit, and publishes the composed node to a store. The recipe editor is a
  bill-of-materials builder: pick the menu item from the tenant's catalog (no raw ULID), add ingredient
  lines each with a per-unit amount in that ingredient's unit, and set the auto-86 threshold — a recipe
  is keyed by its item, so saving is an upsert and the item is fixed once it exists. ULIDs stay behind
  a Technical-details disclosure, destructive actions confirm, and outcomes route through toasts, per
  the kit. English and Vietnamese strings both ship. **Upgrade note:** none — dashboard-only, over the
  M6 routes already added.
- **A published `inventory` node finally gives the edge its recipe book** (roadmap v2, Track M6;
  [ADR-0079](docs/adr/0079-inventory-and-suppliers.md)). A new `PUT /admin/config/inventory` route
  (behind `console.config.publish`, audited by count only) assembles a tenant's authored ingredients,
  recipes, and suppliers into the store's `inventory` config node the same way campaigns/tax/floor
  publish, preserving the store layer's other keys. The edge's config apply parses that node and calls
  `pos_core::inventory::from_published` to build the runtime `RecipeBook` and a per-item auto-86
  threshold map, replacing the empty bootstrap book: a fired line now consumes its bill of materials
  (`decide_line` already reads `session.recipes`). An absent or unparseable node leaves the base book
  untouched — a bad publish never blanks a trading store's recipes. `EdgeSession::item_sellable` is the
  pure §8 auto-86 decision (available-vs-threshold) the edge now holds; driving it from a **live**
  on-hand stock projection is the flagged goods-in/stocktake follow-up, so no trading store's menu is
  86'd from this slice alone. **Upgrade note:** none — additive route and additive session state; no
  protocol, migration, or permission change, and a store with no `inventory` node behaves exactly as
  today (every item unlimited).
- **Ingredients, recipes, and suppliers can be authored over the API** (roadmap v2, Track M6;
  [ADR-0079](docs/adr/0079-inventory-and-suppliers.md)). New `/admin/inventory/*` routes give a tenant
  per-record CRUD over its ingredients, per-item/modifier recipes (bill of materials + auto-86
  threshold), and supplier references, over the `InventoryStore` seam. An ingredient's and a
  supplier's id is server-minted, so a client never supplies or forges one; a recipe's key is the menu
  item it makes, so a recipe is a `PUT` upsert keyed by that item id (`201` when the item had no recipe
  before, `200` when it replaced one). Reads are behind `console.data.read`; every write is behind a
  new `console.inventory.manage` permission (Owner/Admin — the manage norm, since recipes are
  proprietary process; Ops publishes but does not author them) and is audited by summary. The audit
  trail records an ingredient and a supplier in full (reference data) but a recipe only by its item,
  line count, and threshold — never the per-ingredient amounts, which are T2 configuration and stay in
  the inventory store. The typed dashboard client gains the matching methods and types. **Upgrade
  note:** one new console permission (`console.inventory.manage`, granted to Owner and Admin); no
  protocol or migration change in this slice. The composed `inventory` node publish, edge apply, and
  console screens follow in the same track.
- **Inventory authoring has somewhere to live** (roadmap v2, Track M6;
  [ADR-0079](docs/adr/0079-inventory-and-suppliers.md)). A new `InventoryStore` seam persists a
  tenant's ingredients, per-item/modifier recipes (with their auto-86 thresholds), and supplier
  references, backed by an `inventory_items` table (migration `0037`). The three record kinds share
  one shape — a tenant, a stable id, and a document — so they live in one table discriminated by
  `kind` rather than three near-identical tables, each record held as `jsonb` exactly as `campaigns`
  is; CRUD is per-record. Tenant-scoped and RLS-isolated like every other config table, with an
  in-memory fake for the seam tests and a Postgres round-trip integration test (create/replace by
  `(kind, id)`, tenant isolation, kind-scoped delete). Recipe quantities and supplier terms are T2
  configuration; a row carries no customer identifier. The admin routes, publish path, edge apply, and
  console follow in the same track. **Upgrade note:** an additive migration (`0037_inventory`,
  rollback safe); no protocol or permission change in this slice.
- **The inventory engine gets a wire node and a conversion, so recipes can finally be authored**
  (roadmap v2, Track M6; [ADR-0079](docs/adr/0079-inventory-and-suppliers.md)). The `pos-core`
  inventory domain (spec §8 — recipes, stock projection, auto-86) has been built and property-tested
  since P3, but it had no inputs: the edge plumbs a `RecipeBook` into its fire path yet that book is
  empty, so a fired line consumes nothing and every item reads as unlimited. This slice adds the
  authoring shape. A new `pos_proto::inventory::PublishedInventory` config-node type carries a store's
  ingredients (id, name, `UnitOfMeasure`), per-item/per-modifier recipes (ingredient lines with a
  per-unit quantity) and their auto-86 thresholds, and lightweight supplier references — the wire
  mirror the cloud will write as the `inventory` key on the config tree, exactly as `campaigns`/`tax`
  do. `pos_core::inventory::from_published` turns it back into the runtime `RecipeBook` and a per-item
  threshold map — the one place that sees both shapes. A new `UnitOfMeasure` wire enum
  (gram/kilogram/millilitre/litre/piece) is a per-ingredient display label; the availability
  arithmetic stays unit-agnostic, so there is deliberately no cross-unit conversion. The storage,
  admin routes, publish path, edge apply, and console follow in the same track. **Upgrade note:** none
  — additive `pos-proto` types and a new enum; no protocol, migration, or permission change in this
  slice, and a store with no `inventory` node behaves exactly as today (every item unlimited).
- **The console shows reconciliation history and gives the rebuild lever a button** (roadmap v2,
  Track O3; [ADR-0078](docs/adr/0078-sync-and-ota-closure.md)). A new Reconciliation screen reads
  `GET /admin/reconcile` and lists each store's recent reconciliation runs — store, ids offered, how
  many were re-pushed (or "in sync" when none), and when — resolving store names from the registry so
  no raw ULID is front-and-centre, and polling so a fresh run shows without a manual refresh. For the
  store in context it also surfaces the rollup-**rebuild** lever (`POST .../rollups/reset`, ADR-0036)
  that existed only as a route: a confirmed button that resets the cloud's materialised rollups so the
  projector re-folds them from the event log — the recovery ADR-0078 §4 wanted behind a button rather
  than `curl`. The rebuild is behind `console.config.publish` (the server enforces it — a viewer sees
  the history but a rebuild returns `403`); the history read is behind `console.data.read`. Added to
  the Overview nav, with en/vi strings. **Upgrade note:** none — a console-only screen over the
  existing `/admin/reconcile` read and `rollups/reset` lever; no schema, protocol, or permission
  change.
- **Reconciliation now leaves a trail** (roadmap v2, Track O3;
  [ADR-0078](docs/adr/0078-sync-and-ota-closure.md)). The `POST /internal/reconcile` diff
  ([ADR-0040](docs/adr/0040-reconciliation.md)) answered "which of these ids am I missing?"
  statelessly, so a gap it closed was invisible afterwards — nothing recorded that reconciliation
  ran, for which store, or how much it caught. Every diff now appends one run to a history —
  candidates offered, missing found, and when — into a new `reconcile_runs` table (migration `0036`,
  RLS tenant-isolated like `store_liveness`), and `GET /admin/reconcile` lists a tenant's recent runs
  (newest first, optional store filter, behind `console.data.read`). Recording is best-effort: a
  history write that fails never denies the edge the diff it is waiting on. A run is operational
  telemetry — counts and a timestamp, never event contents or a customer identifier — so it stays out
  of the T1/T2 reproduction rules. Wiring the *edge* to send reconciliation manifests on a schedule
  from the shipped `pos_edge` binary stays the flagged hardware/composition gate (ADR-0078 §Deferred,
  ADR-0055); this slice delivers and tests the cloud-side recording and read that a real edge — or the
  existing manual re-push flow — feeds. **Upgrade note:** an additive migration (`0036_reconcile_runs`,
  rollback safe) and a new `/admin/reconcile` read; no protocol or permission change (the read reuses
  `console.data.read`).
- **The console has an OTA updates screen — rollout progress and the levers in one place** (roadmap
  v2, Track O3; [ADR-0078](docs/adr/0078-sync-and-ota-closure.md)). The reporting and lever backends
  landed with no way to see or drive them from the console; this adds the screen. A tenant-scoped
  **rollout progress** table shows, per store, the binary it last reported running, whether its
  post-install self-test passed, when it last reported, and whether it is online — the rollout-ring
  progress that was invisible — and polls so it stays current. For the store in context, a **manage
  rollout** pane reads the store's published rollout and offers the first-class levers: a form that
  publishes a rollout from typed fields (target version, ring, ramp percent, signing key, revoked
  keys) and a kill switch that halts (or resumes) it without re-typing. Both writes go through the
  server's `console.ota.publish` gate — a viewer sees the progress but a publish returns `403` — and
  the publish is guarded by a confirmation that names the version, ring, and store. Added to the
  Overview nav for Owner/Admin. **Upgrade note:** none — a console-only screen over the existing
  `GET /admin/fleet` read and the `/admin/config/ota` levers; no schema, protocol, or permission
  change (the `console.ota.publish` permission shipped with the levers).
- **First-class OTA rollout levers replace hand-editing a config node** (roadmap v2, Track O3;
  [ADR-0078](docs/adr/0078-sync-and-ota-closure.md)). Publishing a fleet update used to mean typing a
  raw `fleet_update` JSON node into the generic config-tree editor; a fat-fingered ring or ramp went
  live with no affordance to stop it. Three typed routes now stand in front of that node.
  `PUT /admin/config/ota` publishes a rollout from named fields — target version, minimum ring,
  ramp percent, signing key, and revoked keys — composing the `fleet_update` node and running it
  through the same `CapabilityValidator` the generic publish used, so a malformed rollout is still a
  `422` with the exact violations and nothing changes. `POST /admin/config/ota/halt` is the kill
  switch: it loads the store's published rollout, flips `halted`, and re-publishes — an operator
  pauses (or resumes) a bad rollout without re-typing the target, ring, and key, and it is a `400`
  when there is nothing published to halt. `GET /admin/config/ota` reads the currently-published
  rollout. All three preserve the store's other Store-level keys (`menu`, `tax`, `campaigns`, …) the
  way every node publish does. The writes are behind a new **`console.ota.publish`** permission
  (Owner/Admin only — above the `PublishConfig` norm that includes Ops, because pushing a binary to
  the fleet is not a day-to-day publish) and are audited (`config.ota.publish` / `config.ota.halt`
  record the target and ring, never a customer identifier); the read is behind `console.data.read`.
  The dashboard OTA view that drives these follows in the same track. **Upgrade note:** one new
  console permission (`console.ota.publish`, granted to Owner and Admin); no schema or protocol
  change, and the generic config editor still works, so an existing `fleet_update` node keeps
  publishing exactly as before.
- **The cloud ingests OTA reports and shows each store's running version** (roadmap v2, Track O3;
  [ADR-0078](docs/adr/0078-sync-and-ota-closure.md)). The reporting half of the OTA loop now lands
  somewhere: `POST /internal/ota/report` (a trusted-network `/internal` route, the reporting partner
  of `/internal/ingest`) records the version a store is running and its last self-test outcome onto
  the O1 fleet-liveness read model — a report is another kind of liveness contact, so it extends
  `store_liveness` (migration `0035`, additive columns) rather than opening a new table, and advances
  the store's `last_seen_at` too. The fleet read (`GET /admin/fleet` and the per-store detail) now
  carries `installed_version`, `self_test_ok`, and `reported_at`, so the console can finally show, per
  store, what binary it is running and whether its self-test passed — the rollout-ring progress that
  was invisible. A dedicated `OtaReportStore` write seam keeps this off the `ConfigTreeStore` trait;
  the server stamps the report's arrival from its own clock. First-class OTA publish/kill-switch
  levers and the dashboard OTA view follow in the same track. **Upgrade note:** an additive migration
  (`0035_ota_report`, rollback safe) and a new `/internal` route; a store that never reports simply
  leaves the new columns NULL, exactly as the fleet read tolerated before — no protocol or permission
  change.
- **The edge can report an update's outcome to the cloud (`CloudSync::report`)** (roadmap v2, Track
  O3; [ADR-0078](docs/adr/0078-sync-and-ota-closure.md)). OTA was a publish into silence: the cloud
  pushed a rollout as config and the edge decided, installed, self-tested, and rolled back — and told
  no one, so no one could see rollout-ring progress. This adds the reporting half of the loop. The
  `CloudSync` port (ADR-0053) grows a third method, `report(&UpdateReport)` — a port-local struct
  carrying which store reported, the version it is now running, and whether the post-install self-test
  passed — in the port's existing style (like `ActivationGrant`, it names only ids the port already
  uses plus the two facts the cloud needs, never a customer identifier). The `cloud-sync-http` adapter
  posts it to `POST /internal/ota/report` (a trusted-network `/internal` route carrying the identity
  in the body, exactly like `/internal/reconcile`); the fake accepts it; the shared contract suite
  gains a fifth case, so every `CloudSync` implementation is held to it. A report is fire-and-forget:
  one that does not reach the cloud is retried or dropped, never a reason to undo an install that
  already happened. The cloud-side ingest, the OTA-progress read model, and first-class OTA
  publish/kill-switch levers follow in the same track; wiring the edge OTA updater to *call*
  `report()` inside the shipped binary stays the flagged hardware/OS composition gate ADR-0055 and
  `docs/roadmap.md` P9 already hold behind a real box. **Upgrade note:** none — an additive port
  method (`/internal/ota/report` is a new route; a store that never reports is simply unknown to the
  new read model, exactly as before), no protocol, migration, or permission change in this slice.
- **Campaigns are now authorable over the finished pricing engine** (roadmap v2, Track M3;
  [ADR-0077](docs/adr/0077-campaigns-and-scheduling.md)). The pricing engine (`pos_core::campaign`,
  `docs/pos-spec.md` §7) already evaluates all five campaign kinds with windows, quotas, and
  exclusions, but `Campaign` is a pure runtime type with no wire form, so nothing could author or
  deliver a promotion. This track closes that: `pos_proto::campaign::PublishedCampaigns` is the wire
  mirror the cloud publishes as the `campaigns` config-tree node, and
  `pos_core::campaign::campaigns_from_published` is the total, infallible conversion back into the
  `Campaign`s the edge evaluates, so what an operator authors is exactly what the store prices against
  (channels ride as `Open<SalesChannel>` so a newer cloud's token round-trips rather than failing the
  node). A tenant-scoped `campaigns` table (migration `0032`, RLS on `app.tenant_id`) and a
  `CampaignStore` seam hold the authored promotions; per-campaign CRUD routes — `GET`/`POST` on
  `/admin/campaigns` and `GET`/`PUT`/`DELETE` on `/admin/campaigns/{id}` — read behind
  `console.data.read` and write behind a **new `console.campaigns.manage`** permission (Owner/Admin),
  validating each campaign's shape and auditing `campaign.create`/`update`/`delete` with a summary
  (id, name, kind, priority) that never reproduces the discount terms in the queryable trail. The id
  is server-owned. `PUT /admin/config/campaigns` (behind `console.config.publish`, like every other
  node publish) assembles the tenant's campaigns into the `campaigns` config-tree node, and the edge's
  `session_from_config` parses it into `EdgeSession.campaigns` under the never-blank rule — a bad or
  absent publish never drops a trading store's promotions. Voucher batch generation, effective-dated
  scheduling, publish preview/diff, and the dashboard screen follow in the same track. **Applying**
  the delivered campaigns to a live bill (building the eval context, the timing split, and voucher
  redemption) is a flagged follow-up — it is a runtime-pricing change distinct from authoring, and a
  partial version would break §7's one-line-per-campaign rule. **Upgrade note:** an additive migration
  (`0032_campaigns`, rollback safe) and one new permission (`console.campaigns.manage`, Owner/Admin);
  no protocol change (the `campaigns` node is an additive Store-layer key), and a store with no node
  published runs no promotions, exactly as today.
- **Voucher batch generation for a voucher-kind campaign** (roadmap v2, Track M3;
  [ADR-0077](docs/adr/0077-campaigns-and-scheduling.md)). `POST /admin/campaigns/{id}/vouchers` mints a
  batch of up to 10,000 unique, high-entropy codes (12-character Crockford base32, so a code read off a
  printed flyer is unambiguous) for a voucher-kind campaign, stores each as a voucher instance
  (migration `0033_vouchers`, tenant-scoped, `(tenant_id, code)` unique), and returns them once for
  distribution; `GET` lists a campaign's codes. Both are behind `console.campaigns.manage` — a code
  carries redeemable value, so even listing is a manage action, not a plain read — and the mint audits
  `voucher.batch.generate` with the **count only, never the codes**. Unlike an API key, a voucher code
  is stored in clear text because it must be handed out; redemption stays the engine's existing online
  check-and-mark (the `PromotionVoucher*` events), and the atomic redeem endpoint is a flagged
  follow-up. **Upgrade note:** an additive migration (`0033_vouchers`, rollback safe); no protocol or
  permission change (reuses `console.campaigns.manage`).
- **Effective-dated & scheduled config publishes — the Tết-menu case** (roadmap v2, Track M3;
  [ADR-0077](docs/adr/0077-campaigns-and-scheduling.md)). Every other publish is immediate; a Tết menu
  or a midnight price change needed a human awake at 00:00. Now `POST /admin/config/campaigns/schedule`
  snapshots the tenant's campaigns and schedules them to publish to a store at a future instant, a
  background **activator** applies every due publish at its time (through the same config tree the
  immediate publishes use, versioned identically), `GET /admin/config/scheduled` lists a store's
  pending publishes, and `DELETE /admin/config/scheduled/{id}` cancels one. Snapshot-at-schedule, not
  recompute-at-fire: the value published is what was authored and reviewed when scheduled, so later
  edits never leak into a publish nobody looked at again. The mechanism is node-agnostic (`node_key`
  names any Store-layer key), so menu/tax scheduling reuse the same store and activator — this track
  wires the campaign schedule route. Behind `console.config.publish` (schedule/cancel) and
  `console.data.read` (list), audited `config.campaigns.schedule` / `config.schedule.cancel`. The
  activator reports its own task health like the retention and alert loops. **Upgrade note:** an
  additive migration (`0034_scheduled_publishes`, rollback safe) and a new config knob
  `scheduled_publish_interval_secs` (default 30); no protocol or permission change.
- **Publish preview — see the exact diff before committing a campaigns publish** (roadmap v2, Track
  M3; [ADR-0077](docs/adr/0077-campaigns-and-scheduling.md)). Publishing the `campaigns` node was
  all-or-nothing with no before/after. Now `POST /admin/config/campaigns/preview` runs the publish as
  a dry-run — it assembles the same node, composes and validates it against the store's live config
  exactly as the real publish would, and returns the **RFC 7386 merge patch** it would apply to the
  effective document, plus the version it diffs against and an `unchanged` flag — **without minting a
  version, saving, or auditing** (it changes nothing). A candidate that would fail validation comes
  back `422` with the very violations a real publish would reject it with, so an author sees the
  problem before committing. The dry-run helper is node-agnostic (it previews any Store-layer key),
  so scheduled and other node publishes can reuse it. Behind `console.config.publish`, the same
  audience that can publish. **Upgrade note:** none — a new read-only route, no migration, protocol,
  or permission change.
- **Campaigns console screen — author, publish, preview and schedule promotions without JSON**
  (roadmap v2, Track M3; [ADR-0077](docs/adr/0077-campaigns-and-scheduling.md)). The dashboard gains a
  **Campaigns** screen (on the F2 CRUD kit) that ties the whole track together: author the five kinds
  with their discount (percentage or amount off), conditions (minimum bill, sales channels, a weekly
  window), exclusion group and quota; mint voucher batches for a voucher-kind campaign (shown once,
  with a copy-now note); and publish the tenant's campaigns to a store — immediately, after a
  **preview** that shows the exact merge patch, or **scheduled** to a future instant, with the store's
  pending schedule listed and cancellable. Campaigns are tenant-authored; publishing/previewing/
  scheduling need a store chosen in the top bar (the F0 context gate), and the screen sits behind the
  Owner/Admin nav gate that matches `console.campaigns.manage`. Full en + vi localisation.
  **Upgrade note:** none — a console-only addition over the routes already shipped in this track; no
  migration, protocol, or permission change.
- **PDPD/GDPR subject-request tooling — per-subject lookup, export, and erasure**
  (roadmap v2, Track M5, slice 7; [ADR-0076](docs/adr/0076-subject-request-tooling.md)). The Data
  Protection contact's instrument for an individual rights request, over the existing subject store
  ([ADR-0035](docs/adr/0035-retention-and-pii-masking.md)). `GET /admin/subjects/{id}` looks a subject
  up — existence, whether it is masked, and the field count, **without** returning the values; `GET
  .../export` returns the record with its field values (the portability/access payload); `POST
  .../erase` masks every field permanently, keeping the id so records still reconcile. All three are
  **per-subject and tenant-scoped** (there is no list-all or bulk route), behind a new **owner-only**
  `console.subjects.manage` permission, and audited — the entry records who acted on which subject and
  what action, the export records the field count, **never the field values**. The dashboard adds an
  owner-only **Subject requests** screen with a standing reminder to confirm the lawful basis and
  identity and to escalate EU-resident requests to the Data Protection contact; erase requires typing
  the subject id to confirm. The `SubjectStore` seam gains a per-subject `fetch`; erase reuses the
  retention masking. **Upgrade note:** additive routes + one owner-only permission; no schema (reuses
  the `subjects` table), wire, or edge change.
- **A CSV import rail for the translation grid — dry-run first, apply on confirm**
  (roadmap v2, Track M5, slice 6; [ADR-0075](docs/adr/0075-media-and-file-rail.md)). `POST
  /admin/translations/import/dry-run` parses an uploaded CSV and returns a **row-by-row report**
  (would-create / would-update / rejected-with-reason) **without writing anything**; `POST
  /admin/translations/import/apply` re-parses the same file and merges the valid rows onto the tenant's
  grid — existing keys not in the file are preserved, rejected rows (empty key, or missing the `en`
  fallback) are skipped — then saves and audits the row counts (never the contents). Both are behind
  `console.translations.manage`, under a 4 MB body limit, and the merged grid satisfies the `en`-fallback
  rule by construction. The Translations screen gains an **Import CSV** button that shows the dry-run
  report in a review dialog before the operator confirms. Round-trips with the slice-5 translation export.
  Item import (upsert with FK validation) and the T1/T2 domains are deferred (ADR-0075 decision 5).
  **Upgrade note:** additive routes; no schema, wire, or edge change.
- **A CSV export rail — download the catalog items and the translation grid**
  (roadmap v2, Track M5, slice 5; [ADR-0075](docs/adr/0075-media-and-file-rail.md)). A reusable, pure CSV
  serialiser (`pos_cloud::export`, buying the `csv` crate for RFC-4180 quoting per ADR-0007) behind two
  authenticated routes: `GET /admin/catalog/export/items` (behind `console.catalog.manage`) streams the
  item master as `items.csv` — id, name, status, tax class, category/sub-category, image ref, **never a
  price** — and `GET /admin/translations/export` (behind `console.translations.manage`) streams the
  translation grid as `translations.csv` (a `key` column plus one column per locale). Each is audited —
  the entry records who exported which domain and how many rows, never the row contents — and is
  tenant-scoped by RLS, so it is not a cross-tenant path. The dashboard adds an **Export CSV** button to
  the Menu (items) and Translations screens. Per the data-classification guardrails (ADR-0075 decision
  5), the **employee roster (T1)**, **per-channel prices (T2 verbatim)**, and the rollup report export
  are deliberately **not** shipped here — they await a human-reviewed design/DPIA. **Upgrade note:**
  additive read-only routes; adds the `csv` dependency; no schema, wire, or edge change.
- **A Media library screen and an image picker on the item editor**
  (roadmap v2, Track M5, slice 4; [ADR-0075](docs/adr/0075-media-and-file-rail.md)). A new **Media**
  screen (Master data, owner/admin) lists the tenant's uploaded images as a thumbnail grid with size
  and upload date, uploads a new image, and deletes one — with a confirm step and the never-blank
  posture (a deleted image an item still references shows a placeholder, never a broken image). The
  Catalog item editor gains an **image** column: a compact widget shows the item's current image and
  lets the operator pick one from the library or upload a new one, or remove it; the change persists
  immediately. Both surfaces are tenant-scoped and gate their write affordances on `console.media.manage`
  (owner/admin), which the server re-checks. New i18n keys (en + vi). **Upgrade note:** dashboard-only;
  no API, schema, or edge change.
- **A catalog item can carry an image — an optional `image_ref` on the item**
  (roadmap v2, Track M5, slice 3; [ADR-0075](docs/adr/0075-media-and-file-rail.md)). `CatalogItem` gains
  an `image_ref: Option<MediaId>` (additive column, migration 0031), authored in the console and
  round-tripped through the catalog CRUD routes and the typed dashboard client. This is an
  authoring/display concern only: the compiled `MenuBook` the edge reprices from is unchanged — images
  do not cross to the edge in this track. A reference to a media asset that has since been deleted is not
  an error: the ref simply resolves to nothing and the UI shows a placeholder (the never-blank posture).
  The brand-logo `image_ref` follows with the receipt/branding work, beside its renderer. **Upgrade
  note:** additive, forward-only migration applied on boot; the create/update item request bodies gain an
  optional `image_ref` field (absent leaves the item imageless); no wire or edge change.
- **Media upload, serve, list, and delete routes — the image pipeline's first caller**
  (roadmap v2, Track M5, slice 2; [ADR-0075](docs/adr/0075-media-and-file-rail.md)). `POST /admin/media`
  takes an image as a raw binary body (under an 8 MB limit), re-encodes it through the ADR-0042 pipeline,
  and stores only the two bounded JPEG renditions (the original is never persisted); `GET /admin/media`
  lists summaries; `GET /admin/media/{id}/thumbnail` and `.../detail` stream one rendition as
  `image/jpeg` with an immutable cache header; `DELETE /admin/media/{id}` removes an asset. Upload and
  delete are behind a new **`console.media.manage`** permission (Owner/Admin, deny-by-default) and
  audited (`media.upload`, `media.delete`); reads and the serve routes need only `Read`. A non-image
  upload is a `400`, an image that cannot be reduced within budget a `422`. The typed dashboard client
  gains an upload core plus `uploadMedia` / `listMedia` / `deleteMedia` and rendition-URL helpers.
  **Upgrade note:** additive routes + one new permission; no schema, wire, or edge change.
- **Media storage foundation — image renditions in Postgres `bytea` behind a `MediaStore` seam**
  (roadmap v2, Track M5, slice 1; [ADR-0075](docs/adr/0075-media-and-file-rail.md)). The ADR-0042 image
  pipeline (`images::render`, ≤30 KB thumbnail / ≤150 KB detail) gains its storage: a new tenant-scoped
  `media_assets` table (migration 0030) holds the two JPEG renditions as `bytea` — per ADR-0042/ADR-0031,
  Postgres, not the condemned `blob-garage` port. A `MediaStore` seam (`put` / `get` one rendition /
  `list` summaries without the bytes / `delete`) lands with a `store-postgres` `PostgresMedia` adapter
  and the `pos-cloud` persistence bridge, plus an in-memory fake and unit + Postgres integration tests
  (bytea round-trip, single-rendition read, tenant isolation, delete). Media is immutable — insert,
  read, list, delete; no update. **Upgrade note:** additive, forward-only migration applied on boot; no
  routes or edge/wire changes yet (the upload/serve routes and image fields land in later slices).
- **Per-locale item names, end to end — authored, compiled, and rendered in the store's language**
  (roadmap v2, Track M4, slice 8; [ADR-0074](docs/adr/0074-localization-and-tax.md)). A catalog item
  gains an optional `name_translations` map (locale code → name), authored in the Catalog screen's item
  editor (one input per shipped locale) and stored as a jsonb column on `catalog_items` (migration
  0029, additive, `NOT NULL DEFAULT '{}'`). The menu compiler carries them onto the compiled
  `MenuEntry` as an **additive** `display_name_translations` map beside the existing `display_name`,
  which stays the fallback. A store's optional **display language** — a new field on the `locale` config
  node, set from the Store settings screen — selects each item's name once at the edge, at config
  install (`MenuCatalog::localized`), folding it into `display_name` so the priced line, receipt, and
  KDS read in the store's language with the reprice contract untouched. An item with no translation for
  that language keeps its default name (never-blank), and a store that sets no language behaves exactly
  as before. **Upgrade note:** wire-additive — a new optional field, nothing renamed or removed, so
  `PROTOCOL_VERSION` is unchanged and an older edge simply ignores the field; the migration is
  forward-only and applied idempotently on boot. Prices and names are T2 catalog content, shipped in
  config and never logged.
- **The Store settings console screen — currency, timezone, cutoff, and a live business-date preview**
  (roadmap v2, Track M4, slice 9; [ADR-0074](docs/adr/0074-localization-and-tax.md)). A new store-scoped
  `/store-settings` screen (under *Settings*) sets a store's currency (3-letter code, with the country
  registry's currencies as suggestions), IANA timezone (with a short list of common zones as
  suggestions, any valid zone accepted — the server validates against the tz database), and
  business-date cutoff hour, then publishes them with `publishLocale`. A live **business date now**
  preview computes the store's trading day from the chosen timezone and cutoff (mirroring the edge's
  `derive_business_date`, ADR-0014), so the operator sees the effect before publishing; an unknown
  timezone is flagged inline. **Upgrade note:** dashboard-only; consumes the M4 slice-5 locale publish
  route.
- **The translation grid gains dynamic locales, per-locale completion %, and a missing-only filter**
  (roadmap v2, Track M4, slice 7; [ADR-0074](docs/adr/0074-localization-and-tax.md)). The grid's
  columns are no longer the app's own UI locales (`en`/`vi`): they are the union of the platform's
  content locales (`GET /admin/locales`, driven by the country modules), the enforced `en` fallback,
  and any locale the stored grid already carries — so a value authored in a locale the UI does not yet
  ship (`ja`, `ko`) is visible and editable. Each column header shows that locale's completion
  percentage (non-empty cells ÷ keys), and a *show only keys with a missing translation* toggle filters
  the grid to the gaps. No schema or wire change — the persisted grid was already locale-agnostic and
  `en` stays the enforced floor. **Upgrade note:** dashboard-only.
- **The Tax rates console screen — author the (class × channel) grid and publish it** (roadmap v2,
  Track M4, slice 6; [ADR-0074](docs/adr/0074-localization-and-tax.md)). A new tenant-scoped
  `/tax-rates` screen (under *Settings*) edits the tenant's tax rates as a grid — rows are the tenant's
  tax classes, columns the sales channels, each cell a rate in percent. A blank cell means the class is
  not sold on that channel (the edge refuses such a sale rather than charging no tax), stated inline so
  the meaning is not a surprise. **Save** writes the whole table (`setTaxRates`); **Publish to store**
  pushes it to the store in the top-bar context as the `tax` config node (`publishTax`), enabled only
  when a store is selected. Loads on the scoped tenant context (F0 gate), reuses the shared channel
  labels. **Upgrade note:** dashboard-only; no protocol, schema, or permission change.
- **Store locale settings — currency, timezone, and business-date cutoff, published and applied**
  (roadmap v2, Track M4, slice 5; [ADR-0074](docs/adr/0074-localization-and-tax.md)). `PUT
  /admin/config/locale` (behind `console.config.publish`, audited `config.locale.publish`) validates a
  store's currency (3-letter code), **IANA timezone** (against the tz database), and business-date
  cutoff hour (`0..=23`) with the domain constructors — a bad value is a `400` naming it — then writes
  them as the store's **`locale`** config node, preserving sibling nodes. The edge's
  `session_from_config` gains a `locale` branch that applies each field to `EdgeSession`'s currency,
  timezone, and cutoff **independently** — a malformed field leaves that one setting as-is rather than
  resetting a trading store's clock to UTC — killing the hardcoded VND/UTC/04:00 bootstrap so business
  dates derive in the store's real timezone (ADR-0014). The typed client gains `publishLocale`.
  **Upgrade note:** additive Store-layer node and route; a store with no `locale` node keeps today's
  behaviour; no protocol change.
- **Countries & locale packs surfaced as master data** (roadmap v2, Track M4, slice 4;
  [ADR-0074](docs/adr/0074-localization-and-tax.md)). `pos-cloud` now builds a `CountryRegistry` from
  the country modules compiled into it — a country is a Cargo feature like the edge (ADR-0027), and the
  cloud enables the reference `zz` module by default (a fork adds `country-vn = ["dep:pos-country-vn"]`).
  Two read-only routes behind `console.data.read`: `GET /admin/countries` lists each module (code,
  display name, currency, preferred language, number format, default retention) and `GET /admin/locales`
  lists the content locales the platform can serve (each module's preferred language plus the enforced
  `en` fallback) — the source the currency picker and the translation grid's column set read from. The
  typed client gains `listCountries` / `listLocales` and a `Country` type. Production country modules
  with fiscalization (VN e-invoice, JP qualified invoice) remain a flagged follow-up. **Upgrade note:**
  additive read routes and an additive default feature; no schema or protocol change.
- **The `tax` config node — authored rates reach the edge and are billed** (roadmap v2, Track M4,
  slice 3; [ADR-0074](docs/adr/0074-localization-and-tax.md)). Closes the headline M4 gap: a store now
  bills the *authored* tax rates instead of the hardcoded bootstrap default. `PUT /admin/config/tax`
  (behind `console.config.publish`, audited `config.tax.publish`) assembles the tenant's authored
  `(tax class × channel)` rates into a `TaxRateTable`, writes it as the store's **`tax`** config node on
  the Store layer — preserving the sibling nodes (`menu`, `layout`, `permissions`, `floor`, capability
  flags) — and versions it through the config tree. The edge's `session_from_config` gains a `tax`
  branch that parses the node into `EdgeSession::tax_rates`, which `pos_core::billing` and repricing
  already read; an absent or unparseable node leaves the running table untouched (the never-blank rule),
  and `rate_for` still refuses an unpriced class rather than charging no tax. The typed client gains
  `publishTax`. **Upgrade note:** additive Store-layer node and route; a store with no `tax` node keeps
  today's behaviour; no protocol change.
- **The tax-rate console API — author the rates the edge applies** (roadmap v2, Track M4, slice 2;
  [ADR-0074](docs/adr/0074-localization-and-tax.md)). `GET /admin/catalog/tax-rates?tenant_id=` lists a
  tenant's authored `(tax class × channel)` rates (behind `console.data.read`); `PUT
  /admin/catalog/tax-rates` replaces the whole table (behind `console.catalog.manage`, the permission
  tax *classes* already use) and is audited (`tax_rate.set`). Every row is validated **before** the
  write — its class must be one the tenant has authored, its channel a token this build knows, its rate
  no more than 100% (`10000` bps), and no `(class, channel)` pair repeated — so a bad grid returns a
  `400` naming the fault rather than reaching the store as a `500`. The typed dashboard client gains
  `listTaxRates` / `setTaxRates` and a `TaxRate` type. **Upgrade note:** additive route on an existing
  permission; nothing publishes the rates to a store yet (the `tax` config node lands in the next slice).
- **Tax-rate authoring, foundations — a home for the per-(tax class × channel) rate** (roadmap v2,
  Track M4, slice 1; [ADR-0074](docs/adr/0074-localization-and-tax.md)). Begins Track **M4
  (Localization & tax)**. Tax *calculation* has been built and applied on the edge since ADR-0028
  (`pos_proto::TaxRateTable`, `pos_core::billing`, `EdgeSession::tax_rates`) — but the rate a tax class
  resolves to was only ever the hardcoded bootstrap default, with no storage, editor, or publish path.
  This slice adds the store: migration `0028_catalog_tax_rates.sql` (one tenant-scoped, RLS-isolated row
  per `(tenant, tax class, sales channel)`, the rate in basis points bounded `[0, 10000]`); a
  `TaxRateStore` seam (`list` / wholesale `set`) with an in-memory fake and a `store-postgres`
  `PostgresTaxRates` adapter that replaces a tenant's whole table in one transaction; and a `to_table`
  helper that assembles authored rows into the wire `TaxRateTable` the edge reprices from (a missing
  rate stays a visible `None`, never a silent zero-tax sale). ADR-0074 (added here) records the M4
  design and its deferred items (receipt templates depend on M5; production country modules with
  fiscalization). The routes, publish, and editor land in the following slices. **Upgrade note:**
  additive migration only; nothing authors or reads these rows yet.
- **The Alerts console screen — the operator's live view of the alert engine** (roadmap v2, Track O2,
  slice 6; [ADR-0073](docs/adr/0073-alerting.md)). A new fleet-wide `/alerts` screen (grouped under
  *Overview*, beside Audit, no working context required) reads the alerts the evaluator maintains: the
  active set by default, with a toggle to *Recent* history (active + resolved). Each row shows the
  severity (Critical rides a `danger` pill; the label always accompanies the hue), the localized
  condition kind, the server's summary, the scope (a tenant or *Fleet-wide*), the last-seen age, and
  the lifecycle state (firing / acknowledged / resolved); a details drawer opens the full timestamps
  and the alert's `detail` numbers, with the ULID and dedup key tucked behind *Technical details*. An
  operator holding `console.alerts.manage` (Owner/Admin/Ops) gets **Acknowledge** and **Resolve**
  actions inline and in the drawer — hidden for read-only roles, exactly as the server gates them; both
  are idempotent, so a stale click is harmless. Built on the F2 kit (`DataTable` search/sort/paginate,
  `Drawer`, `StatusBadge`, `EmptyState`, `TechnicalDetails`), with `StatusBadge` gaining a `danger`
  tone (`text-danger`, which clears WCAG-AA on the card surface) for an active fault. The
  notification-bell → live-alert-count wiring is a small, separable follow-up. **Upgrade note:**
  dashboard-only; no protocol, schema, or permission change.
- **The console alert API — the in-console delivery channel** (roadmap v2, Track O2, slice 4;
  [ADR-0073](docs/adr/0073-alerting.md)). The alerts the evaluator maintains are now readable and
  actionable from the console: `GET /admin/alerts` lists the fleet-wide active set (or recent history,
  active + resolved, with `?recent=true&limit=`) behind `console.data.read`; `POST /admin/alerts/{id}/ack`
  and `POST /admin/alerts/{id}/resolve` acknowledge and hand-resolve an alert behind a new
  **`console.alerts.manage`** permission (Owner/Admin/Ops — alerts are a day-to-day ops concern) and are
  audited (`alert.acknowledge`, `alert.resolve`). Both writes are idempotent, and a condition still
  firing simply reopens on the next evaluator tick. The typed dashboard client gains `listAlerts`,
  `acknowledgeAlert`, and `resolveAlert`; route tests cover the active/recent split, the ack/resolve
  lifecycle, and the session gate. The **in-console channel is the shipped delivery path**; the
  off-console push channels (a webhook channel over the ADR-0032 transport, email, and a Zalo/Telegram
  chat adapter) plug into an `AlertChannel` seam and are a flagged follow-up. **Upgrade note:** additive
  read + two idempotent write routes and one new console permission; no schema or protocol change.
- **The alert evaluator background loop** (roadmap v2, Track O2, slice 3;
  [ADR-0073](docs/adr/0073-alerting.md)). The cloud now *watches* its read models: a new background
  loop (in the mould of the rollup projector and webhook dispatcher) sweeps the fleet, task-health, and
  webhook read models each tick, runs the pure evaluator, and reconciles the firing set against the
  alert store — opening a new alert per newly-firing condition, refreshing live ones, and **resolving
  those that have cleared** (a store coming back online clears its own offline alert). It records its
  own `task_health` tick, so the watcher is itself watched, and is added to the health endpoint's
  expected-loops set. Cadence and every threshold are `CloudConfig` tunables (`alert_eval_interval_secs`,
  `alert_store_offline_secs` = 5 min, `alert_relay_backlog_max`, `alert_relay_oldest_secs`,
  `alert_projector_stale_slack_secs`, `alert_jetstream_capacity_percent`) with sensible defaults. Live
  conditions: store offline, relay backlog, webhook auto-disabled, projector unhealthy. The JetStream
  capacity condition is supported by the evaluator but its cloud-side stream-info probe is a flagged
  follow-up (`NatsConsumer` exposes no capacity read and NATS ingest is optional). **Upgrade note:**
  additive cloud-internal loop + config tunables; no schema, protocol, or permission change; the loop
  writes alerts to the `0027` table but nothing reads them until the console API lands (slice 5).
- **Alert store: the `alerts` table and its seam** (roadmap v2, Track O2, slice 2;
  [ADR-0073](docs/adr/0073-alerting.md)). Persists operational alerts with an open→resolved lifecycle.
  Migration `0027_alerts.sql` adds the `alerts` table — nullable `tenant_id` (server-wide alerts carry
  none), RLS-isolated like `audit_log`, with a **partial unique index** (`coalesce(tenant_id,''), kind,
  dedup_key WHERE resolved_at IS NULL`) that enforces one *open* alert per condition while letting a
  resolved one remain as history. The new `AlertStore` seam upserts (opens or refreshes in place,
  keeping the original id and `first_seen_at`), resolves, acknowledges, and lists the active and recent
  alerts; a `store-postgres` `PostgresAlerts` adapter implements it (the upsert's `ON CONFLICT` targets
  the partial index), with an in-memory fake proving the lifecycle contract and a real-Postgres
  integration test covering dedup, resolve→history, and reopen. An alert stores a condition and a small
  JSON detail, never a payload or PII. **Upgrade note:** additive migration `0027` (rollback-safe); no
  protocol or permission change; nothing writes to the table until the evaluator loop lands (slice 3).
- **Alerting foundations: the alert model and a pure evaluator** (roadmap v2, Track O2, slice 1;
  [ADR-0073](docs/adr/0073-alerting.md)). Begins Track **O2 (Alerting)** — the cloud finally *watches*
  the read models O1 built instead of only serving them on demand. This first slice lays the type
  foundations: `pos_cloud::alerts` defines the `AlertKind` catalogue (store offline, relay backlog,
  webhook disabled, projector unhealthy, JetStream near-capacity), an ordered `AlertSeverity`, and the
  `FiringAlert` a detector produces; and a **pure `evaluate`** that turns a read-model snapshot
  (per-tenant fleet rows + disabled webhook endpoints, background-task health, an optional JetStream
  capacity reading, and tunable thresholds) into the firing set — no I/O and no clock, so every
  condition is exhaustively unit-tested. ADR-0073 records the engine's design (open→resolved lifecycle,
  delivery over the console + the existing webhook transport, email/chat as seams) and, explicitly,
  which conditions ship on ready data now versus those deferred for want of upstream telemetry (clock
  drift, disk-low, print-error spike, e-invoice/fiscal, projector failure *streaks*). Store, background
  loop, delivery, and the `/admin` surface land in the following slices. **Upgrade note:** additive
  cloud-internal types only; nothing runs yet — no schema, route, protocol, or permission change.
- **The in-store app draws the store's real floor and routes fires by the published plan** (roadmap
  v2, Track M2, slice 8; [ADR-0072](docs/adr/0072-floor-and-kitchen.md)). The in-store `ui/` now reads
  `GET /api/floor` at start and renders the store's published areas and tables instead of a hardcoded
  eight, keeping a never-blank fallback (the default grid stays until a plan syncs, and if a store has
  none published). Firing and bumping route to the store's published default station rather than the
  old fixed `S01` — and the edge now **derives a fired line's station from the published routing**
  (wiring `EdgeSession::resolve_station`, previously defined but unused): `fire_line` resolves the
  station from the line's item/course rules and falls back to the caller's station only when the store
  has no station plan yet, so a device fires a line without needing to know the kitchen's stations. The
  edge's `POST /api/lines/{id}/fire` accepts an optional `station_id` (absent lets the plan decide),
  and a fire that resolves to no station at all is a `409` (`UnroutableLine`) rather than a blank
  route. **Upgrade note:** edge + in-store-UI only; `FireRequest.station_id` is now optional (an absent
  field is accepted); no schema or `PROTOCOL_VERSION` change.
- **Floor & kitchen console screens** (roadmap v2, Track M2, slice 7;
  [ADR-0072](docs/adr/0072-floor-and-kitchen.md)). Two new master-data screens on the F2 CRUD kit,
  both per-store and behind the working store in the top bar. **Floor** manages a store's areas and
  tables (create/rename/archive areas; create/edit/archive tables by label, area, seat count, and an
  optional grid column/row — the visual pointer-drag editor stays deferred to F3), publishes the plan
  with one action (`publishFloor`), and renders the printable QR sheet (`tableQrTokens`), degrading to
  a gentle "not configured" note when the cloud has no table-token secret rather than an error.
  **Kitchen stations** manages stations (name, optional backup for printer failover, a catch-all
  default flag) and item→station routing rules (the item is picked from the tenant's catalog, so no
  course ULID is ever typed), and publishes the same plan. Both screens gate every write affordance on
  `console.floor.manage` (Owner/Admin) — the server re-checks — and route outcomes through the F1
  toast. New `/floor` and `/stations` nav entries (Master data group) with English + Vietnamese
  strings. **Upgrade note:** dashboard-only; no schema, protocol, or permission change.
- **Table QR-token minting** (roadmap v2, Track M2, slice 6;
  [ADR-0072](docs/adr/0072-floor-and-kitchen.md), [ADR-0057](docs/adr/0057-qr-ordering.md)). Wires the
  previously-orphaned `mint_table_token`: `GET /admin/floor/qr` mints the signed QR token for each of a
  store's active tables so the console can print a QR sheet. The token binds `(tenant, store, table)`
  — no PII — and is the same value `verify_table_token` already checks on a guest order, so QR ordering
  now closes end-to-end. Behind `console.data.read` and wired only when a table-token secret is
  configured (the same gate as the guest `POST /v1/qr/orders`). The typed client gains `tableQrTokens`.
  The printable QR sheet UI lands with the Floor screen (slice 7). **Upgrade note:** additive read
  route; no schema or protocol change.
- **The edge applies the `floor` & `stations` nodes** (roadmap v2, Track M2, slice 5;
  [ADR-0072](docs/adr/0072-floor-and-kitchen.md)). `EdgeSession` gains `floor`/`stations` fields (with
  `with_floor`/`with_stations` builders), and `session_from_config` rebuilds them from the pulled
  document's `floor`/`stations` nodes with the same **never-blank** gate the `menu`/`permissions`
  branches keep — an absent or malformed node leaves the plan unchanged. `EdgeSession::resolve_station`
  derives a fired line's station from the published routing (a thin delegate to
  `pos_core::floor::route_station`), so the store can route by rule instead of trusting the caller. A
  new `GET /api/floor` serves the live floor plan + kitchen stations to the in-store UI (which renders
  its real tables and routes fires in the next slice). **Upgrade note:** edge-only; the store now
  applies a published floor plan and station routing where before it knew neither.
- **Publish the `floor` & `stations` config nodes** (roadmap v2, Track M2, slice 4;
  [ADR-0072](docs/adr/0072-floor-and-kitchen.md)). `POST /admin/floor/publish` compiles a store's
  areas/tables into a `FloorPlan` and its stations/routing into a `StationPlan` (only active records;
  a stale backup or a rule to a removed station is dropped, so the plan is consistent by
  construction), runs the §10 referential validation (`pos_core::floor`), and — only if valid — merges
  both onto the store's `floor` and `stations` config nodes and versions them through the config tree,
  preserving the other Store-level keys (`menu`/`layout`/`permissions`/capability flags). An invalid
  plan is a `422` with the violated rules, never a stored state. Behind `console.config.publish`,
  audited `floor.publish` (counts only). The typed client gains `publishFloor`. **Upgrade note:**
  additive route + two new config nodes; no `PROTOCOL_VERSION` change (the edge starts reading them in
  the next slice).
- **Floor & kitchen `/admin` CRUD routes** (roadmap v2, Track M2, slice 3;
  [ADR-0072](docs/adr/0072-floor-and-kitchen.md)). `GET/POST /admin/floor/areas`,
  `GET/PATCH /admin/floor/areas/{id}`, the same for `/admin/floor/tables`, `/admin/kitchen/stations`,
  and `GET/POST /admin/kitchen/routing` + `DELETE /admin/kitchen/routing/{id}`. Reads are behind
  `console.data.read`; every write is behind a new **`console.floor.manage`** permission (Owner/Admin)
  and is audited (`floor.area.*`, `floor.table.*`, `kitchen.station.*`, `kitchen.routing.*`; floor data
  is not PII). A routing rule is rejected unless it matches exactly one of an item or a course — the
  same §10 rule the publish validator enforces, surfaced early as a `400`. The typed dashboard client
  gains the matching `Area`/`FloorTable`/`Station`/`RoutingRule` types and list/create/update/remove
  methods. **Upgrade note:** additive routes + one new console permission; no schema or protocol change.
- **Kitchen master-data store: stations & routing rules** (roadmap v2, Track M2, slice 2b;
  [ADR-0072](docs/adr/0072-floor-and-kitchen.md)). Migration `0026_kitchen_stations_and_routing.sql`
  adds the per-store `kitchen_stations` (name, optional backup station for printer failover, an
  `is_default` catch-all flag) and `station_routing_rules` (a fired line matched by item or course →
  a station, in author-controlled `sort` order) tables, RLS-isolated like the floor. Stations are
  archived-never-deleted; a routing rule is a mapping that is *removed* (like an assignment). New
  `StationStore` / `RoutingRuleStore` seams + `store-postgres` adapter methods + an in-memory fake and
  seam test. Not PII. **Upgrade note:** additive migration `0026` (rollback-safe); no protocol change.
- **Floor master-data store: areas & tables** (roadmap v2, Track M2, slice 2a;
  [ADR-0072](docs/adr/0072-floor-and-kitchen.md)). A store's floor is now persisted master data:
  migration `0025_floor_areas_and_tables.sql` adds the `floor_areas` and `floor_tables` tables —
  per-store (a floor is a physical room, so both carry `store_id` beside `tenant_id`), RLS-isolated on
  `app.tenant_id`, and archived-never-deleted like every other registry entity. New `AreaStore` /
  `TableStore` seams (with an in-memory fake for tests and a `store-postgres` `PostgresFloor` adapter)
  create/list/get/update areas and tables; a table carries a label, seat count, and an optional grid
  position for the visual editor. Not PII. Seams and storage only in this slice; the CRUD routes,
  publish, and console follow. **Upgrade note:** additive migration `0025` (rollback-safe); no protocol
  change.
- **Floor & kitchen master-data types** (roadmap v2, Track M2, slice 1;
  [ADR-0072](docs/adr/0072-floor-and-kitchen.md)). Foundations for a store's floor plan and kitchen
  routing: a new `AreaId` (a named region of the floor), and two shared `pos-proto` config shapes the
  cloud authors and the edge reads through one type — `FloorPlan` (areas, each with its tables: a
  label, seat count, and optional grid position) and `StationPlan` (kitchen stations, each with an
  optional backup station, plus item→station routing rules and a default station). `pos-core::floor`
  adds the pure referential validation (`floor_violations`/`station_violations` — table ids unique
  across the floor, a routing rule names a known station and matches exactly one of an item or a
  course, a backup names a known different station) the cloud will run before publishing, and
  `route_station`, the resolver that turns a fired line into its station (item rule → course rule →
  default). Types and logic only in this slice; the schema, publish, edge read, and console land in
  the following M2 slices. **Upgrade note:** additive proto types; no schema or `PROTOCOL_VERSION`
  change.
- **A form-driven capability editor on the Config screen** (roadmap v2, Track M8, slice 3;
  [ADR-0071](docs/adr/0071-config-without-json.md)). The Config screen now offers the §10 capability
  catalogue as labelled toggles seeded from the store's current effective profile, the three presets
  (full-service / counter / retail) as one-click buttons, an **inline conflict preview** of the §10
  inter-flag rules (a violation shows the instant a toggle creates it, not as a `422` on publish), and a
  **diff of the flags that will change** before publishing. Publishing goes through a new
  `PUT /admin/config/capabilities` that **merges only the named flag booleans into the store's Store
  config layer** — the node-merge the catalog/people publishes use, so the store's other config
  (`menu`, `layout`, `permissions`) survives — and versions it through the config tree, which re-runs
  the §10 rules so an invalid combination is a `422`, never a stored state. The publish button is
  disabled while a conflict stands; the write is behind `console.config.publish` and is audited
  (`config.capabilities.publish`; flags are not PII). Raw-JSON publish stays for everything a form does
  not yet cover. **Upgrade note:** additive route + console UI; no schema or `PROTOCOL_VERSION` change.
- **The console can read the capability catalogue** (roadmap v2, Track M8, slice 2;
  [ADR-0071](docs/adr/0071-config-without-json.md)). A new `GET /admin/capabilities` serves the §10
  capability flags (key, default, one-line description), the three presets (full-service / counter /
  retail, each as its set of enabled flag keys), and the inter-flag rules (id + description) — all from
  `pos-core`'s own catalogue, behind `console.data.read`. This is what the Config screen's form editor
  (slice 3) renders toggles and previews conflicts from, so the console never hard-codes a flag list or
  a rule. **Upgrade note:** additive read-only route; no schema or `PROTOCOL_VERSION` change.
- **The edge now applies published capability flags (a fixed silent no-op)** (roadmap v2, Track M8,
  slice 1; [ADR-0071](docs/adr/0071-config-without-json.md)). Publishing a store's capability profile
  (§10 — `tables_enabled`, `pay_first_enabled`, `kds_enabled`, …) from the console had no effect on the
  running store: the edge's config-pull rebuild reconstructed only the `menu` and `permissions` nodes
  and never read the flag keys, so turning off table service or switching a store to pay-first was a
  no-op. Now `session_from_config` rebuilds the store's `CapabilityContext` from the pulled document's
  flag keys — via a new shared `CapabilityContext::from_flags` reader in `pos-core` that the cloud
  validator and the edge both use, so the two cannot disagree on what a profile means. A publish that
  names at least one flag is authoritative (an unnamed flag takes its declared default); a publish that
  names none leaves the store's profile unchanged (the same never-blank contract the menu/permissions
  branches keep). **Upgrade note:** edge-only correctness fix; no schema, config-node, or
  `PROTOCOL_VERSION` change — the flags were already in the effective document; the edge simply starts
  reading them.
- **The edge applies the published `permissions` node — staff sign in against the cloud's set**
  (roadmap v2, Track M1, slice 6; [ADR-0070](docs/adr/0070-people-and-access.md)). The edge's
  config-pull rebuild now reads the `permissions` node into a `StaffRoster` on the `EdgeSession` (each
  badge `code` → the granted permission set + the Argon2id PIN hash), and
  `EdgeSession::authorise_staff(code, pin)` verifies a sign-in against the published hash offline
  ([ADR-0030](docs/adr/0030-pairing-and-offline-auth.md)), returning the person's `PermissionSet` on
  success — so the store authorises staff from the console's published set rather than a local roster.
  A permission id the running edge predates is dropped (forward-compatible), and an absent or
  malformed node leaves the roster unchanged — the same safe-by-default rebuild as the menu. This
  completes Track M1 (people & access). **Upgrade note:** edge-only; no `PROTOCOL_VERSION` change (the
  node rides the existing config tree).
- **Publish a store's people to its `permissions` config node** (roadmap v2, Track M1, slice 5;
  [ADR-0070](docs/adr/0070-people-and-access.md)). A pure compiler turns a store's active assignments
  into the flat, edge-shaped `permissions` document — per staff member: `id`, `code`, `name`, the
  granted permission set (the assignment's role flattened to its `pos-core` ids, deduped and sorted),
  and the **Argon2id PIN hash** the edge verifies against offline ([ADR-0030](docs/adr/0030-pairing-and-offline-auth.md))
  — archived employees dropped, staff sorted by code so the output is byte-stable. `POST
  /admin/people/publish` writes it onto the store's `permissions` config node and versions it through
  the config tree, so it rides to the store like every other config change ([ADR-0033](docs/adr/0033-config-tree.md))
  — no new channel. Behind `console.config.publish`; the audit records the config version and staff
  count, never a name or PIN. The People screen gains a **Publish to store** action. **Upgrade note:**
  additive route + config node; no `PROTOCOL_VERSION` change (the node rides the existing config tree).
- **A People console screen — manage employees, roles, PINs, and store assignments** (roadmap v2,
  Track M1, slice 4; [ADR-0070](docs/adr/0070-people-and-access.md)). A new tenant-scoped `/people`
  screen on the F2 CRUD kit: an employee roster (with create, archive/restore, and a set/reset-PIN
  dialog whose digits are sent once and never shown again — the list shows only whether a PIN is
  *set*); a role editor (a drawer of the `pos-core` permission catalogue grouped by area, so the
  operator picks capabilities, never types a permission string); and per-store assignments (assign a
  person to the store chosen in the top bar, with a role, and remove to offboard). All write
  affordances are gated on the operator holding `console.people.manage` (owner/admin); reads open to
  any admin (the server re-checks every route). Wired into the nav under Master data and the typed
  API client. **Upgrade note:** dashboard-only; no API or schema change beyond slice 3's routes.
- **`/admin` console API for employees, roles, and assignments — every write audited** (roadmap v2,
  Track M1, slice 3; [ADR-0070](docs/adr/0070-people-and-access.md)). The write and read surface the
  People console will use: `GET/POST /admin/employees`, `GET/PATCH /admin/employees/{id}`,
  `PUT /admin/employees/{id}/pin` (set/reset — the PIN is Argon2id-hashed server-side and never
  returned, stored raw, or logged), `GET/POST /admin/roles`, `GET/PATCH /admin/roles/{id}`,
  `GET/POST /admin/assignments` (list by `?store_id=` or `?employee_id=`),
  `DELETE /admin/assignments/{id}`, and `GET /admin/people/permissions` (the `pos-core` catalogue the
  role editor offers, so the console never invents a permission string). A role's permission set is
  validated against that catalogue before it is stored. A new **`console.people.manage`** permission
  (Owner/Admin) gates every write; reads need only `console.data.read`. Every write emits an audit
  entry ([ADR-0069](docs/adr/0069-audit-trail.md)) that records the employee **id, code, status, and
  role — never the name, and never the PIN or its hash** (a test asserts the trail never contains the
  name, the PIN, or an `argon2` hash). **Upgrade note:** additive routes only; no `PROTOCOL_VERSION`
  change; one new console permission (`console.people.manage`).
- **Role templates and per-store staff assignments (foundation)** (roadmap v2, Track M1, slice 2;
  [ADR-0070](docs/adr/0070-people-and-access.md)). Two new tenant-scoped, RLS-isolated tables
  (migration `0024`) that give a store's employees their *access*: `role_templates` — a tenant's
  named roles (e.g. *Cashier*, *Manager*), each a stored subset of the **`pos-core` permission
  catalogue** (§9) held as a `jsonb` id set; and `employee_store_assignments` — the join binding a
  person to one of their tenant's stores with a role. New `RoleTemplateStore` and `AssignmentStore`
  seams (create/list/get/update roles; assign/list-by-store/list-by-employee/remove assignments) with
  `store-postgres` adapters and in-memory fakes. The console never invents a permission string: a new
  `permission_catalogue()` exposes the `pos-core` catalogue (id, group, risk, PIN policy, description)
  and `is_known_permission()` validates a stored subset against it. Roles are **archived, never
  deleted** (no `DELETE` grant), like employees; an assignment is a plain grant that is **removed**
  (offboarding a person from a store), so it alone carries a `DELETE` grant. None of this is PII
  beyond the employee row itself — a role is names + permission ids, an assignment is three ids. No
  routes or UI yet (M1 slice 3 adds `/admin` CRUD + audit). **Upgrade note:** one additive,
  forward-only migration `0024_role_templates_and_assignments`; no `PROTOCOL_VERSION` or permission
  change.
- **The cloud can now record a store's employees (foundation)** (roadmap v2, Track M1, slice 1;
  [ADR-0070](docs/adr/0070-people-and-access.md)). The console's first record of *who works at a
  store* — and the console's first **T1 Restricted / Vietnam-PDPD-scoped** data. A new tenant-scoped,
  RLS-isolated `employees` table (migration `0023`) holds only what access control needs: a minted
  `id`, the owning tenant, a `name`, the tenant-unique staff `code`, a status, and `pin_phc` — the
  **Argon2id** hash of the set PIN, `NULL` until set and **never the PIN itself**. A new
  `EmployeeStore` seam (create / list / get / update / set-or-reset PIN) with a `store-postgres`
  adapter and an in-memory fake; a read exposes only `has_pin`, never the hash. Employees are
  **archived, never hard-deleted** (no `DELETE` grant), so history and any published permission set
  stay reconcilable and erasure is handled through the Data Protection contact ([ADR-0035](docs/adr/0035-retention-and-pii-masking.md)),
  not an ad-hoc delete. This is access management, not employee monitoring — no contact, biometric,
  behavioural, or location data. No routes or UI yet; this is the store the later M1 slices (role
  templates, per-store assignments, `/admin` CRUD, People screen, and the `permissions` config node
  the edge applies) build on. **PDPD note:** a deployment storing employee PII must confirm its
  lawful basis, staff notification, retention period, and DPIA — surfaced by the console, not presumed
  by the code. **Upgrade note:** one additive, forward-only migration `0023_employees`; no
  `PROTOCOL_VERSION` or permission change.
- **Detail views now show the entity's own audit history, and who resolved device proposals**
  (roadmap v2, Track G, G2 slice 6; [ADR-0069](docs/adr/0069-audit-trail.md)). A reusable
  `AuditTrail` panel reads an entity's history from the same `GET /admin/audit` (filtered by entity
  type and id) and shows who created it, who last changed it, and the entries between — surfacing
  `created/updated at·by` straight from the trail rather than from denormalized columns. It is wired
  into the Fleet store drawer (a store's change history) and the Devices screen (a "recently
  resolved" panel naming who approved or rejected each proposal — the `resolved_by` the roadmap
  calls for). The panel drops into any Detail view. **Upgrade note:** no schema, API, or permission
  change — it reuses the slice-4 read and `console.data.read`.
- **A store's config now has a version history you can view, diff, and roll back** (roadmap v2,
  Track G, G2 slice 5; [ADR-0033](docs/adr/0033-config-tree.md),
  [ADR-0069](docs/adr/0069-audit-trail.md)). The config tree already kept every published version;
  this surfaces it. `GET …/config/versions` lists them newest-first (each version's instant read from
  its own ULID, and the current one flagged), `GET …/config/versions/{id}` returns a past version's
  effective document for a diff, and `POST …/config/rollback` restores a chosen version — **append-only**:
  it re-publishes that version's effective config as a *new* current version, so nothing in the
  history is altered and the store pulls the restored config on its next sync. Rollback is audited as
  `config.rollback`. The Config screen gains a version-history list with a per-version view, a
  "compare with current" line diff, and a confirm-gated roll-back. **Upgrade note:** no schema,
  `PROTOCOL_VERSION`, or permission change — the reads reuse `console.data.read` and rollback reuses
  `console.config.publish`; the version log is the config tree's existing persisted state. Rollback
  restores the *effective* configuration exactly; per-level layer attribution is flattened into the
  base layer (documented in ADR-0069).
- **The console has an Audit screen: a filterable, fleet-wide record of who changed what** (roadmap
  v2, Track G, G2 slice 4; [ADR-0069](docs/adr/0069-audit-trail.md)). Now that the write routes emit
  entries, `GET /admin/audit` reads them back — newest first, behind `console.data.read` (every
  console role) — with server-side filters for entity type, action, acting admin, and a time window;
  an absent `tenant_id` is the fleet-wide read (every tenant, including tenant-global entries), a
  present one scopes to that tenant and excludes the global rows. Filters run in SQL before the row
  limit, so a narrow filter still reaches older matches. A new **Audit** screen (Overview nav) lists
  the trail in the F2 kit's `DataTable` with entity/action/actor filters, and a detail drawer shows
  each change's before/after JSON, actor, and instant; the entry's ULID stays behind Technical
  details. The `AuditStore` seam gained a `query` read (an `AuditQuery` filter) alongside the plain
  `list`. **Upgrade note:** no schema, `PROTOCOL_VERSION`, or permission change; reuses migration
  `0022` and the existing `console.data.read` permission. Config version list/diff/rollback and the
  per-Detail audit tab are the next G2 slices.
- **The rest of the console's write surface now records to the audit trail** (roadmap v2, Track G,
  G2 slice 3; [ADR-0069](docs/adr/0069-audit-trail.md)). Extending slice 2 beyond the org registry,
  every remaining `/admin` mutation now appends one `audit_log` entry on success: API-key issue and
  revoke, webhook register/delete/re-enable, config publish, rollup reset, admin invite/role/status
  and invite revoke, device-proposal approve/reject (the `resolved_by` is the acting admin), the
  translation-grid save, and the whole catalog authoring surface (item, tax class, item and display
  categories/sub-categories, modifier group, menu, section, layout button, placement — create,
  update, and remove) plus the menu publish. The recorder reaches the sub-routers (device,
  translation, catalog, catalog-publish) as the same object-safe `Arc<dyn AuditRecorder>` carried on
  their state. **Secrets are never recorded:** an API-key entry captures the granted scopes and
  expiry and a webhook entry the endpoint URL, but the key token and HMAC signing secret are
  excluded; admin-management entries carry the target admin's id and the role/status set, not an
  email. Idempotent no-ops (revoking an already-gone key, resolving an already-resolved proposal)
  record nothing. Recording stays best-effort after the write. **Upgrade note:** no schema,
  `PROTOCOL_VERSION`, or permission-identifier change; reuses migration `0022`. No customer or
  employee personal data is recorded. The break-glass reset tombstone (written from the `reset_admin`
  binary, not an `/admin` route) and the audit read API + screen are the next G2 slices.
- **The org-registry write routes now record to the audit trail** (roadmap v2, Track G, G2 slice 2;
  [ADR-0069](docs/adr/0069-audit-trail.md)). Building on the slice-1 store, every successful
  tenant/brand/store/device create or update under `/admin` now appends one `audit_log` entry — the
  acting admin snapshotted from the session, the `resource.verb` action (`tenant.create`,
  `store.update`, `device.create`, …), the affected entity's id, and the new value as `after`. The
  recorder is carried as an object-safe `Arc<dyn AuditRecorder>` shared across the router and the
  registry sub-router, so a handler emits without threading a store generic through the router types;
  a create scopes its entry to the entity's owning tenant (a tenant create to the *new* tenant's id,
  so a tenant's audit tab opens with its own creation). Recording is **best-effort after the write**:
  an audit-store failure is logged, never surfaced, so a mutation the operator asked for and that
  succeeded is never failed by its audit write. Reading the mutation's prior value into `before` and
  covering the remaining `/admin` write surfaces (keys, webhooks, config, catalog, admin management,
  the break-glass reset) are the next G2 slices. **Upgrade note:** no schema, `PROTOCOL_VERSION`, or
  permission-identifier change; reuses migration `0022`. No customer or employee personal data is
  recorded — entries capture administrative actions on business metadata, keyed to console operators.
- **The foundation for a console audit trail — an append-only record of who changed what** (roadmap
  v2, Track G, G2 slice 1; [ADR-0069](docs/adr/0069-audit-trail.md)). Until now none of the ~60
  console write routes recorded anything; G1 gave every operator a distinct identity, and this lands
  the store that finally makes an accountable trail possible. A new append-only `audit_log` table
  (migration `0022`) holds one row per console mutation — the acting admin *snapshotted* at action
  time (id, email, role, so renaming or deleting an admin later never rewrites history), the action
  (`resource.verb`), the affected entity, the before/after as `jsonb`, and the instant — with a new
  `AuditStore` seam (`append` + a recent-first, tenant-scoped `list`). Append-only is enforced at the
  grant (`SELECT`/`INSERT` only, never `UPDATE`/`DELETE`) and rows are RLS-isolated by tenant, with
  tenant-global actions (console admin management, the break-glass reset) carried as `NULL`-tenant
  rows visible only to the trusted connection. This slice is the seam and table; threading the actor
  through the write routes and emitting entries are the next G2 slices. **Upgrade note:** one additive,
  forward-only,
  append-only migration `0022_audit_log`; no `PROTOCOL_VERSION` or permission-identifier change. No
  customer or employee personal data is recorded — the log captures administrative actions on business
  metadata (store names, config, keys), keyed to console operators.
- **The console has a Fleet screen: every store's liveness and the background workers' health at a
  glance** (roadmap v2, Track O, O1 slice 5; [ADR-0068](docs/adr/0068-fleet-liveness.md)). A new
  tenant-scoped `/fleet` screen presents the two O1 reads in one operational view: a system-health
  strip (the rollup projector, retention sweep, and webhook dispatcher, each healthy or not, with when
  it last ran) above a table of the tenant's stores — online/offline, last seen, config in-sync or
  behind, and relay backlog with the oldest pending order's age — plus a per-store detail drawer. It
  polls every 15 seconds so "online" and "last seen" stay current without a manual refresh, hides the
  store ULID behind a Technical-details disclosure, and is fully localized (en/vi). Built on the F2
  CRUD kit; read-only. **Upgrade note:** none — a frontend screen over the existing `/admin/fleet` and
  `/admin/health/tasks` reads; no API, migration, or permission change.
- **The console can see whether the background workers are alive and keeping up** (roadmap v2, Track
  O, O1 slice 4; [ADR-0068](docs/adr/0068-fleet-liveness.md)). The cloud's off-request loops — the
  rollup projector, the retention/PII sweep, the webhook dispatcher — now record a heartbeat at the
  end of every tick (a `task_health` row with the instant and a small detail: whether the tick's work
  succeeded, its configured interval, a count or two). `GET /admin/health/tasks` reports each loop the
  deployment turned on, deriving a health verdict at read time — a loop is unhealthy if it has gone
  quiet (last tick older than its interval times a few cycles of slack), never ticked at all (dead
  since boot, surfaced rather than hidden), or ticked recently but its last pass failed. Behind the
  existing `console.data.read` permission (every console role). **Upgrade note:** one additive,
  forward-only migration `0021_task_health` (fleet-wide server state — no `tenant_id`, no RLS, like
  `super_admin`); no `PROTOCOL_VERSION` or permission-identifier change, and `/admin` stays absent
  from the public OpenAPI.
- **The console can read the fleet: which stores are online, in sync, and how deep their order
  backlog is** (roadmap v2, Track O, O1 slice 3; [ADR-0068](docs/adr/0068-fleet-liveness.md)).
  `GET /admin/fleet?tenant_id=…` lists every store of a tenant, and `GET /admin/fleet/{store_id}`
  reads one, as a read-only join across four tables the operator would otherwise have to correlate by
  hand: the registry `stores` row (name, status), `store_liveness` (last-seen, held version, last
  config pull), the config tree (the currently-published version), and the order-relay queue (pending
  backlog + oldest-pending age). Each row carries two verdicts derived at read time so the answer is
  always current with no background sweep: `online` (`now − last_seen_at` within a 180-second
  freshness window — a few pull/heartbeat cycles of slack) and `config_current` (the held version
  equals the published one). A store that is un-configured, never-seen, or has an empty queue simply
  reports `null`/`0` in those fields. Both routes are behind the existing `console.data.read`
  permission, so every console role — Owner, Admin, Ops, Viewer — can watch the fleet. **Upgrade
  note:** none — read-only `/admin` routes over existing tables; no migration, `PROTOCOL_VERSION`, or
  permission-identifier change, and `/admin` stays absent from the public OpenAPI.
- **A store can send a lightweight liveness heartbeat** (roadmap v2, Track O, O1 slice 2;
  [ADR-0068](docs/adr/0068-fleet-liveness.md)). `POST /sync/stores/{id}/heartbeat` — authenticated
  with the same scoped `read_config` API key the config pull uses — advances the store's
  `last_seen_at` and nothing else, so a store that is up but not currently pulling config (a parked
  long-poll, a quiet spell between publishes) still reads as online. It preserves the version and
  config-pull instant a prior pull recorded; a heartbeat-only store's row carries those `NULL`. Unlike
  the config-pull capture, recording is the request's whole purpose, so a store-write failure answers
  `503` for the edge to retry rather than being swallowed. The edge gains a thin, seam-tested
  heartbeat loop (`heartbeat_client`) that pings on a fixed interval, mirroring the config-pull loop;
  its real HTTPS sender wires in alongside the config-transport handoff. **Upgrade note:** none —
  additive route on the existing `store_liveness` table; no migration, `PROTOCOL_VERSION`, or
  permission-identifier change.
- **The cloud now records when each store was last seen and which config version it holds** (roadmap
  v2, Track O, O1 slice 1; [ADR-0068](docs/adr/0068-fleet-liveness.md)). An edge pulls its
  configuration on a loop (`GET /sync/stores/{id}/config`), carrying the version it currently holds;
  the handler used that only to decide up-to-date-vs-deliver and then discarded it, so the cloud never
  knew whether a store was actually there. Each pull now upserts a `store_liveness` row — the contact
  instant, the version the edge reported holding, and the pull instant — the smallest durable record
  the forthcoming fleet overview reads to show which stores are online and on the current config. The
  write is **best-effort**: a liveness-store failure is logged and swallowed, never failing the config
  pull the store needs. It is captured through the `ConfigTreeStore` seam (the one the pull path
  already holds) into a table kept deliberately separate from `config_trees`, so a store with no
  published config — and, in a later slice, one that only heartbeats — still registers as seen.
  Online/offline is derived at read time, not stored. **Upgrade note:** additive migration
  `0020_store_liveness` (RLS-isolated by tenant like `config_trees`); no `PROTOCOL_VERSION` change
  (the edge already sends the held version), no permission-identifier change.
- **The back office now has screens for admins, invitations, sessions, and account security**
  (roadmap v2, Track G, G1 slice 7 — the last G1 slice; [ADR-0067](docs/adr/0067-multi-admin-console-rbac.md)).
  Three console screens put the multi-admin surface built in slices 3–6 in front of an operator, on
  the F2 CRUD kit: **Admins** (owner/admin) lists the roster, invites by email and role — showing the
  single-use copy-invite-link once — lists and revokes pending invites, and (owner only) changes a
  role or suspends/reactivates an admin, with the last-active-owner and no-self-management guardrails
  reflected in the UI; **My sessions** (every admin) lists the caller's own live sessions with the
  current one flagged and protected, revokes one, or signs out everywhere else; **My security** (every
  admin) re-enrols the authenticator after re-confirming the password and (re)generates the one-time
  recovery codes, each shown once. A new **`GET /admin/whoami`** returns the acting admin's own
  identity (id, email, name, role, status — never a credential) so the nav gates the roster to
  owner/admin; the gating is a convenience only, as the server re-checks every route's permission. A
  public **`/invite`** page lets an invitee redeem their link and self-enrol (choose a password, add
  the returned TOTP secret) with no prior session, mirroring first-boot `/setup`. **Upgrade note:**
  none — one additive read-only endpoint (`/admin/whoami`) and console UI only; no schema, protocol,
  or permission-identifier change.
- **A console admin can now see and revoke their own sign-ins, and idle sessions time out** (roadmap
  v2, Track G, G1 slice 4; [ADR-0067](docs/adr/0067-multi-admin-console-rbac.md)). `GET
  /admin/sessions` lists the acting admin's live sessions — each with the client IP and user-agent it
  was minted for, when it was signed in, and which one is the current request — `DELETE
  /admin/sessions/{handle}` revokes one, and `POST /admin/sessions/revoke-others` signs out every
  other device ("sign out everywhere else"). These are self-service: any authenticated admin manages
  their **own** sessions regardless of role, and every operation is scoped to the caller, so no admin
  can list or revoke another's session. The session handle is the hash of the token (never the token,
  and not reversible to it), so listing it grants no capability. Sessions now carry a **sliding idle
  TTL**: a real request extends the session by the idle window (default 30 minutes,
  `admin_session_idle_ttl_secs`), up to an **absolute cap** (the existing `admin_session_ttl_secs`,
  default 8 hours — now the hard ceiling, unchanged in value). A session left idle past the window
  expires even with the console tab open, because the lightweight "am I signed in?" poll deliberately
  does not slide it; only genuine actions do. **Upgrade note:** additive migration
  `0019_admin_session_sliding.sql` adds two nullable columns to `admin_sessions`; sessions minted
  before it keep their original fixed expiry (they do not slide). A new default value,
  `admin_session_idle_ttl_secs` = 1800s; `admin_session_ttl_secs` keeps its 8-hour default but is now
  interpreted as the absolute cap. No protocol or permission-identifier change.
- **Console admins can now be invited, self-enrol, and be managed** (roadmap v2, Track G, G1 slice 3;
  [ADR-0067](docs/adr/0067-multi-admin-console-rbac.md)). An owner or admin invites by email and role
  (`POST /admin/invites`); the server mints a single-use, TTL-bounded token and returns it once as a
  **copy-invite-link** the inviter hands over out-of-band — no email is sent, and only the token's
  `SHA-256` is stored. The invitee self-enrols (`POST /admin/invites/accept`, token-authenticated, no
  session): they choose their own password and receive a one-time TOTP enrolment, exactly as first-boot
  does, so no one else ever learns their credential. Pending invites list and revoke
  (`GET`/`DELETE /admin/invites`); the admin roster lists (`GET /admin/admins`) and an owner changes a
  role or suspends/reactivates an admin (`PATCH /admin/admins/{id}/role|status`). Guardrails: only an
  owner may invite or create another owner (an admin cannot escalate); the **last active owner** can
  never be demoted or suspended; an address that is already an admin cannot be re-invited; an invite is
  single-use and cannot be replayed. Invite acceptance is invite-token-gated, not session-gated. New
  config `admin_invite_ttl_secs` (default 3 days). **PDPD note:** an admin's email is personal data —
  it is stored and handled **internally only** (no external send under the copy-link model); confirm
  the lawful basis (employment/legitimate interest) and retention for admin records. **Upgrade note:**
  additive — no schema migration (the `admin_invites` table shipped in `0018`); no `PROTOCOL_VERSION`
  change; a new optional config key with a safe default.
- **Console role-based access control is now enforced on every `/admin` route** (roadmap v2, Track G,
  G1 slice 2; [ADR-0067](docs/adr/0067-multi-admin-console-rbac.md)). A fixed console permission
  catalogue and the four role templates (`owner`/`admin`/`ops`/`viewer`) are declared with the same
  compile-forced registry pattern `pos-core` uses for store permissions (`docs/pos-spec.md` §9): a
  permission cannot be added without deciding which roles receive it, and the default is deny. The
  `/admin` session guard is now **role-aware** — it resolves the admin a session belongs to and each
  route declares the permission it requires, so a signed-in but under-privileged admin gets a `403`
  (distinct from the `401` an unauthenticated caller gets). Login stays password + TOTP for now and
  binds the session to the sole `owner`; a session minted before this change (or on a not-yet-seeded
  install) resolves to the owner, so no one is locked out across the upgrade. First-boot enrolment
  now also mirrors the super-admin into `admin_users` as that owner. Owner and admin retain full
  reach (owner alone manages admins); `ops` gets the day-to-day surface (devices, webhooks, config
  publish) but not API keys, org/store creation, catalog authoring, or translations; `viewer` is
  read-only. **Upgrade note:** no visible change yet for a single-owner install (the owner holds every
  permission); the role distinctions take effect once additional admins are invited (a later G1
  slice). Per-admin email-based login also arrives with invitations. No `PROTOCOL_VERSION` change.
- **Multi-admin console identities — schema and storage foundation** (roadmap v2, Track G, G1 slice 1;
  [ADR-0067](docs/adr/0067-multi-admin-console-rbac.md), superseding ADR-0034). The cloud console is
  moving from a single shared super-admin to multiple named admins with least-privilege roles. This
  first slice lands the durable foundation and nothing else: migration `0018` adds `admin_users`
  (per-user Argon2id password + TOTP secret, a unique case-insensitive email, a display name, a role
  of `owner`/`admin`/`ops`/`viewer`, and an `active`/`suspended` status), `admin_invites` and
  `admin_recovery_codes` (each storing only a `SHA-256` of its single-use token/code), and gives
  `admin_sessions` nullable `admin_id`/`ip`/`user_agent` columns for per-admin accountability. The
  existing `super_admin` row is migrated in place into the first `owner` (so an install upgrades
  without losing its credential and there is always at least one owner). The `AdminStore` seam gains a
  multi-admin surface — create/list/get/find-by-email, set-role, set-status, and count-active-owners —
  implemented over `store-postgres` and an in-memory fake, unit-tested for case-insensitive email
  uniqueness, the last-owner count, and credential redaction in `Debug`. **The login flow and session
  guard are unchanged in this slice** (they still read `super_admin`); they migrate onto `admin_users`
  in a later G1 slice, so this ships no behaviour change on its own. **Upgrade note:** one additive
  migration (`0018`), rollback-safe; the migrated owner is seeded with a synthetic, non-routable
  placeholder email (`owner@super-admin.invalid`) that the owner replaces from the console once the
  multi-admin UI lands — the password hash and TOTP secret carry over unchanged, so sign-in is
  unaffected. No `PROTOCOL_VERSION` change; no permission-identifier change yet.
- **The operator console gains a reusable CRUD kit, and the master-data screens are rebuilt on it**
  (roadmap v2, Track F2). A new `dashboard/src/components/kit.tsx` adds `DataTable` (sortable columns,
  empty-state slot, row actions), `Modal`/`Drawer`, a `ConfirmDialog` with optional type-the-name
  gating for destructive actions, `StatusBadge`, `EmptyState`, `FormField`, `TechnicalDetails` (a
  disclosure that keeps an entity's ULID present but out of the way), and a `ReorderList` primitive.
  The five simplest screens now build on it — API keys, Webhooks, Devices, Stores, and (lightly)
  Translations — so every list is a consistent sortable table, every destructive action (revoke,
  delete, reject, archive) goes through a confirmation instead of a one-click button, ULIDs move
  behind a "Technical details" disclosure, and write outcomes surface as toasts. Stores keeps inline
  brand reassignment, and the `DataTable` gained built-in **search + pagination** (client-side, over
  the rows a screen already holds — right-sized for the admin lists' volumes). **Still to come in F2**
  (flagged): AIP-193 errors on `/admin`, read-one endpoints, and `ETag`/`If-Match` concurrency;
  server-side list push-down (with `(tenant_id, created_at)` indexes and `/admin` in the OpenAPI
  drift-gate) is deferred until a list is large enough to need it, rather than churning every store
  seam and the `/admin` response shape now.
- **The admin console gains a framework-standard shell: grouped scope-aware nav, breadcrumbs, a
  command palette, toasts, a notification center, org-switcher search, and locale persistence**
  (roadmap v2, Track F1). The flat ten-item nav is now five labelled groups, each item carrying a dot
  that fills once its working context (tenant, or tenant and store) is set; a breadcrumb strip shows
  tenant › store › page; Cmd/Ctrl-K (and a top-bar button) opens a command palette that jumps to any
  screen by name; a shared toast primitive (`toast.ok`/`toast.error`) surfaces outcomes and keeps a
  short history behind a notification bell; the org switcher filters tenants and stores by name and
  caches the loaded lists across opens; the chosen language is remembered per browser and restored on
  load (falling back to the browser preference, then the `en` floor), the switcher shows each
  language's own name, and the document title and `<html lang>` track the active locale; and a
  version footer names the running build (`VITE_APP_VERSION`). Frontend only; routing unchanged.
  **Deferred within F1** (flagged follow-up): URL-encoded working context (`/t/:tenant/s/:store/…`),
  entity search in the palette (needs the F2 data layer), and in-app links to the shipped operator
  guides (not yet web-served).
- **The context picker can create a tenant, auto-disabled webhook endpoints can be re-enabled, and
  hashed dashboard assets are cached immutably** (Track F, F0). A fresh install's empty registry was a
  dead end — the picker read "No tenants yet." with nowhere to go — so it now carries an inline field
  that mints a tenant, selects it, and loads its (empty) store list, flowing straight on to the first
  store. A webhook endpoint the delivery task auto-disabled after a day of failures can now be
  re-enabled from the Webhooks screen (`POST /admin/webhooks/{id}/enable`, super-admin only,
  tenant-scoped like deletion); delivery resumes from the endpoint's stored cursor, so nothing that
  queued while it was down is skipped. And the embedded dashboard now sets `Cache-Control` — the
  content-hashed bundles under `assets/` as `immutable` for a year, and the `index.html` entry
  document as `no-cache`, so a new deploy is picked up on the next load rather than served stale.
- **The operator UI gains a shared component kit, and the design tokens are drift-guarded** (UX
  polish, WS-E / #104). A `PageHeader` component (the edge counterpart of the dashboard's
  `components/ui.tsx` kit) replaces the `<h1>` markup every edge screen hand-rolled; it takes a `size`
  prop so the KDS and expo screens keep their deliberately larger two-metre titles while the
  operator screens keep theirs — a faithful consolidation, no rendered change (the built CSS is
  byte-identical). The design tokens and the WCAG contrast gate are duplicated verbatim across the
  two separate front-end build roots (`ui/` and `dashboard/`), which cannot share a module; a new
  `cargo xtask mirrored-files` check — in `just preflight` and CI — fails the build the moment a
  mirrored pair diverges, so a token darkened in one root but not the other can no longer silently
  leave one theme failing AA. **Scope:** the deeper visual pass over both surfaces stays human-gated
  (it needs rendered-screen review, like the WCAG *visual* and hardware checks); this lands the
  mechanical, verifiable half.
- **A config publish that carries an unparseable menu is now rejected, not silently dropped**
  (ops hardening, WS-D / #103). The cloud's `CapabilityValidator` — the gate every publish passes,
  including the generic `PUT /admin/stores/{id}/config/{level}` route — now round-trips a `menu` or
  `layout` node through the *exact* path the edge reads them (`to_string` → `from_str`), and rejects a
  publish the store could not consume. Before, a malformed delivery node validated, published, and was
  silently ignored by the edge's forgiving `session_from_config`, leaving a store on its old menu with
  no error anywhere — a "successful" publish that never reached the counter. A `docs/security-review-ws-d.md`
  records the finding (low severity — the route is super-admin-only) and the surrounding review of the
  QR, relay, config and metrics surfaces. Forward-only; no migration, no protocol change.
- **The optional monitoring profile is now wired into `pos_cloud`** (observability, WS-D / #103,
  ADR-0031). A new `[metrics]` config section constructs the `metrics-vm` sink and spawns a sparse
  liveness heartbeat (`pos.cloud.up`) off the sales path, alongside the other background tasks. It is
  **off by default**: per `docs/capacity-and-reliability.md` the monitoring profile stays off below
  ~50 stores in favour of sparse sampling, so a pilot cell leaves `[metrics]` unset and emits nothing.
  The heartbeat carries no labels and no PII, and the sink's bounded queue drops under pressure — a
  metrics backend can never become a trading outage. **Upgrade note:** a new optional `[metrics]`
  config key; absent means the profile is off, so existing deployments are unaffected.
- **A durable KDS bump survives a restart** (WS-D / #103). Projection-rebuild-on-load already replays
  the log on boot (`pos_edge`'s `rebuild`); a regression test now proves the `kitchen.ticket.bumped`
  event folds back on rebuild, so a kitchen screen coming up after a restart does not re-show a ticket
  the kitchen already made.
- **A kitchen-display bump is now durable and agreed across every screen** (P6 residual / #44). A bump
  used to be UI-local — each KDS held its own "done" set, so a second screen or one that reconnected
  never agreed a ticket was made. `POST /api/kds/bump` now records the durable `kitchen.ticket.bumped`
  event and fans it out, and the edge marks a projection set (`Edge::bumped_line_ids`) so the prepared
  lines survive a rebuild. A bump is orthogonal to a line's order state (a made line is still
  `Fired`), so it is written as an event and folded, not as a state-machine transition. The kitchen
  and expo screens fold the same event, so the bumped line drops off every connected screen at once;
  "All away" on the pass bumps a whole table's lines through the one path. A domain test proves the
  event is written, the projection reflects it, and it reaches the fan-out. **Scope:** connected
  screens agree live; seeding a *late-joining* KDS with the already-bumped set on connect rides on the
  same projection-rebuild-on-resync follow-up the rest of the client's live projection waits for
  (`ui/src/App.tsx`). Forward-only and additive; no migration, no protocol change (the event was
  already in the catalogue).
- **A menu published from the dashboard now reaches a trading store's counter without a restart**
  (ADR-0004 cloud-owned config, ADR-0039 config delivery, WS-B / #101). The edge's `EdgeSession` — the
  price book, tax table, capabilities and locale a command reads — is now held behind an
  `RwLock<Arc<…>>` so it can be swapped live: a reader takes a cheap coherent `Arc` snapshot for the
  whole of its command, and `Edge::apply_session` installs a rebuilt one for the next. A new config-pull
  client long-polls the store's effective config from the cloud, rebuilds the session with the pure,
  forgiving `session_from_config` (it reads the compiled `menu` node the catalog publish writes, and an
  absent or malformed node leaves the price book unchanged — a bad publish never blanks a trading
  store), and hot-swaps it into the running edge. The HTTP is a seam (`ConfigTransport`), so the loop is
  tested with no socket, and an integration test proves a live edge that booted with an empty menu
  picks up a published one on the next pull. The whole 60-test edge domain suite still passes over the
  new session cell, unchanged. **Scope:** this rebuilds and hot-swaps the live session; persisting the
  pulled document to the edge's local `ConfigStore` (so a restart keeps the last menu without a
  round-trip), applying the delta form of an update, and reading tax/capability nodes as they gain a
  published shape, layer on this seam. Forward-only and additive; no migration, no protocol change.
- **The store can now pull its order queue from the cloud and report each outcome back** (ADR-0061,
  P11a-2, edge half). A new `pos-edge` relay client long-polls the cloud's per-store queue, feeds each
  pulled order through the store's own `EdgeOrderIn` (which reprices, opens it in the local log, and
  dedupes on the caller's reference — so a redelivery converges on one order in the kitchen), and acks
  the outcome — an `Accepted` record or a typed refusal — back to the cloud. A malformed payload is
  acked as `invalid_argument` rather than dropped, so the cloud stops re-parking it. The HTTP is a
  seam (`RelayTransport`), so the pull→make→ack loop is tested with no socket against the real
  `EdgeOrderIn`; the wire shapes are re-declared to mirror `pos_cloud::relay` (the edge must not depend
  on the cloud crate) and pinned to the cloud's JSON by a round-trip test. The client and its
  background `run` loop are library-ready; wiring the production TLS transport and spawning the loop in
  `serve()` land with the edge config-pull integration (WS-B) that shares the same plumbing.
  Forward-only and additive; no migration, no protocol change.
- **Guests can order from a table QR code, and the public order API is now in the OpenAPI document**
  (ADR-0057/ADR-0012 QR ordering; ADR-0019/ADR-0056 for the doc; P11a-2). A new guest-facing
  `POST /v1/qr/orders` takes an HMAC-signed table token as its only credential — a guest carries no
  API key — verifies it, weighs the store's QR guardrails (offline, business hours, a per-table rate
  limit, staff-confirmation **on by default**) with the existing pure decision, and on acceptance
  forwards the order into the very same relay `POST /v1/orders` uses, on the QR channel for the token's
  table. The guardrail settings come from the store's `qr` config node (all forgiving defaults); the
  per-table rate limit is an in-process sliding window (the cloud is one VPS, ADR-0003). The endpoint
  is off until a `table_token_secret` is configured, the same way `/admin/setup` is gated — a guest
  order to a cloud with no secret is unverifiable, so there is nothing to serve. Separately, the
  public `POST`/`GET /v1/orders` handlers are now annotated for OpenAPI, so `docs/openapi.json`
  registers them (with the `OrderRequest`/`OrderResponse` schemas) alongside the daily-rollups path —
  the generated document, regenerated in this change, is once again the whole external `/v1` contract.
  `FakeIntake`-tested (a signed token is accepted and awaits staff confirmation; a token signed with
  another secret is refused `403` before intake) plus unit tests for the business-hours window and the
  rate limiter. **Upgrade note:** set `table_token_secret` in the cloud config to turn on QR ordering;
  leaving it unset keeps the endpoint off. Forward-only and additive; no migration.
- **A menu can now be organised into sections** (ADR-0066 entity 7, Phase 2a). A `MenuSection`
  (tenant-scoped, per-menu, archived-not-deleted) carries a name and a sort order, behind
  `/admin/catalog/menus/{menu_id}/sections` (list/create/`PATCH`); a placement names the section it
  sits under via a new nullable `menu_section_id`, editable by name in the menu editor and shown as a
  column in the placement list. Backed by migration `0017` (a `catalog_menu_sections` table plus an
  additive nullable column on `catalog_placements`) and the `PostgresCatalog` adapter;
  `FakeCatalog`-tested (a section round-trips and a placement carries its section back), and the
  dashboard typechecks, i18n-lints (en + vi) and builds. **Authoring-only for now:** the compiled
  `MenuBook` is a flat set of entries with no sections, so a section changes only what the operator
  sees while authoring — never what the edge is served. Forward-only and additive; greenfield.
- **The catalog can now author modifier groups — shared option sets attachable to items** (ADR-0066
  entities 4 and 5, Phase 2a). A `ModifierGroup` (tenant-scoped, archived-not-deleted) carries a name,
  a `min_select`/`max_select` choice range, the **member** items that make up its options, and the
  items it is **attached** to — behind `/admin/catalog/modifier-groups` (list/create/`PATCH`), with
  members and attachments authored by name in the menu editor. Backed by migration `0016` (members and
  attachments stored as JSONB arrays of item ULIDs on a single row — no child tables) and the
  `PostgresCatalog` adapter; `FakeCatalog`-tested for the round-trip of members and attachments, and
  the dashboard typechecks, i18n-lints (en + vi) and builds. **Authoring-only for now:** modifier
  groups are captured and published in the cloud but do not yet ride the compiled `MenuBook` to the
  edge — the wire cannot carry a modifier reference on a `MenuEntry` until `pos-proto`/ADR-0063 gains
  that field, which is a flagged follow-up. Forward-only and additive; greenfield.
- **The catalog now has a presentation tier — display taxonomy + per-channel layouts, compiled and
  published beside the price book** (ADR-0066 entities 11 and 12, Phase 2a). The catalog gains a
  **display taxonomy** (`DisplayCategory` / `DisplaySubcategory` — the grouping a screen shows, distinct
  from the operational item taxonomy) and **layout buttons** (an item's button on a channel, under a
  display category/sub-category, with an optional POS grid slot and sort order), behind
  `/admin/catalog/display-categories`, `/admin/catalog/display-subcategories` and
  `/admin/catalog/layout-buttons/{sales_channel}/{menu_item_id}` (list/create/`PATCH`; the button
  keyed by `(channel, item)` with `PUT`/`DELETE`). A new pure `compile_layout_book` folds the buttons
  by `channel → category → sub-category` into a `pos-proto` `LayoutBook`, and **publish now writes it
  onto the store's `layout` config node** alongside the `MenuBook` on `menu` — so a button moving
  reprices nothing and a price change relays no buttons, exactly the separation ADR-0066 requires. The
  layout compiler is forgiving (a button whose category/sub-category is missing or archived is skipped,
  never failing a publish). Backed by migration `0015` and the `PostgresCatalog` adapter. A new
  back-office **Layout** screen (`/layout`, kept deliberately apart from the Menu/pricing screen) edits
  the display taxonomy and, per channel, the item buttons a screen shows — item, display category and
  sub-category, an optional POS grid slot (column/row) and a sort order — all by name; the layout ships
  to the store on the same publish as the price book. `FakeCatalog`-tested (route round-trips) and the
  compiler unit-tested (channel/category/sub-category grouping, archived-category skip, and the
  publish e2e asserting the `layout` node); the dashboard typechecks, i18n-lints (en + vi) and builds.
  Forward-only and additive; greenfield.
- **Items now carry an operational category and sub-category** (ADR-0066 entities 2 and 3, Phase 2a).
  The catalog gains an item taxonomy — `ItemCategory` and `ItemSubcategory` (tenant-scoped,
  archived-not-deleted, a sub-category nested under a category) — behind `/admin/catalog/item-categories`
  and `/admin/catalog/item-subcategories` (list/create/`PATCH`), plus two additive nullable columns on
  `catalog_items` linking an item to its category and sub-category. Backed by migration `0014` and the
  `PostgresCatalog` adapter. The menu editor's item form gains category + (category-filtered)
  sub-category pickers, an editable Categories and Sub-categories card, and a Category column on the
  items table. This is the taxonomy a product-mix report will total by — **distinct** from the
  presentation taxonomy a screen groups by (that is the display taxonomy, entities 11/12). It is
  authoring metadata today; the reporting that consumes it is a later phase. `FakeCatalog`-tested
  (category + sub-category + item linkage round-trips) and typechecked in the dashboard. Forward-only
  and additive; greenfield, no backfill.
- **Tax classes are now named entities, and an item picks one from a list** (ADR-0066 entity 10, Phase 2a).
  A `TaxClass` (id + name, tenant-scoped, archived-not-deleted) joins the catalog store behind
  `/admin/catalog/tax-classes` (list/create, `PATCH` to rename or set status), backed by migration
  `0013` (`catalog_tax_classes`, RLS by tenant) and the `PostgresCatalog` adapter. The menu editor's
  item form now offers a **tax-class dropdown** instead of a pasted `TaxClassId` ULID — closing the
  one raw-ULID entry the editor still had — and the items table shows the class by name. The class is
  country-agnostic; the *rate* for each `(tax_class, channel)` still lives in the store's locale pack.
  `FakeCatalog`-tested (create/list/rename/archive, `404` on an unknown id) and typechecked in the
  dashboard. Migration is forward-only and additive; greenfield, no backfill.
- **The back-office now has a menu editor** (ADR-0066, Phase 2a). A new **Menu** screen (`/catalog`)
  in the cloud dashboard drives the catalog CRUD and publish routes end to end, all by name: create /
  rename / archive **items** (with their tax class); create / rename / **reparent** (inheritance) /
  archive **menus**; open a menu to edit the **items placed in it** — each with a per-channel price
  sheet (dine-in / takeaway / delivery / QR / API, blank = not sold on that channel) and a published
  availability toggle — and finally **publish** a chosen menu to the store in the top-bar context,
  which returns the new config version. Tenant and store come from the existing context picker (no
  ULID typed for either); prices are entered in the currency's smallest unit. All labels run through
  the i18n runtime (English + Vietnamese, `en` the enforced fallback) so the no-hardcoded-strings gate
  stays green. One known gap, called out in the UI: an item's **tax class** is still a pasted ULID —
  a named tax-class picker is a later slice. This completes Phase 2a's operator-facing surface.
- **A menu now publishes to a store — compiled and written onto its config tree** (ADR-0066, Phase 2a).
  A `catalog_publish_router` adds `POST /admin/catalog/publish` behind the super-admin session guard:
  given `(tenant_id, store_id, menu_id)` it loads that tenant's items, menus and placements, runs the
  pure `compile_menu` to a `MenuBook`, and writes the book onto the store's **Store-layer** `menu`
  config node (index 2 in the Tenant→Brand→Store→Device order), re-publishing that whole layer through
  the versioned config tree — so the price book reaches the store exactly like every other config
  change, no new transport. A compiler refusal (unknown or cyclic menu) surfaces as **422**, an
  operator error to fix, distinct from a store 5xx; a store with no tree yet gets one started. Proven
  end to end against the fakes: authoring an item + a dine-in placement, publishing, then reading the
  store's effective config back and finding the compiled `MenuBook` on its `menu` node. Wired into
  `pos_cloud`. Prices stay a T2 asset — compiled and shipped in config, never logged. This closes the
  Phase 2a authoring→compile→publish path; the menu editor UI is the next slice.
- **The catalog is now editable over `/admin`** (ADR-0066, Phase 2a). A `catalog_router` exposes the
  authoring model behind the super-admin session guard: `/admin/catalog/items` and
  `/admin/catalog/menus` (list scoped to `?tenant_id=`, create with the id minted server-side,
  `PATCH` to rename / set status / reparent), and `/admin/catalog/menus/{menu_id}/placements` (list,
  `PUT` to upsert an item's per-channel prices and availability by its `(menu_id, menu_item_id)` pair,
  `DELETE` to remove it). Tenant named the admin-is-global way; every write is `FakeCatalog`-tested for
  create/list/upsert/remove and the session guard. Wired into `pos_cloud`. The publish path (compile →
  config tree) lands in this release; the menu editor UI is the next slice.
- **The catalog authoring model now persists in PostgreSQL** (ADR-0066, Phase 2a). Migration `0012`
  adds `catalog_items`, `catalog_menus` (with an inheritance edge) and `catalog_placements` (an item
  in a menu, its per-channel prices as a `jsonb` document keyed by `(menu_id, menu_item_id)`), all
  RLS-isolated by tenant like the registry tables. A `store-postgres` `PostgresCatalog` adapter and
  the `pos-cloud` `impl CatalogStore for PostgresCatalog` back the seam end to end: items and menus
  create/list/update, placements upsert/list/remove, prices round-tripping through the `text::jsonb`
  cast the config tree already uses. Forward-only and additive (greenfield — no backfill); prices are
  a T2 pricing model, stored only here and in the compiled config, never a log.
- **The menu compiler turns authoring into the flat per-channel snapshot the edge reprices from**
  (ADR-0066, Phase 2a). A pure `compile_menu(items, menus, placements, root)` in `pos-cloud` folds a
  menu's inheritance chain **most-specific-wins** (a child's placement overrides an ancestor's,
  untouched items inherited) and selects each placement's per-channel price into that channel's
  `MenuCatalog`, producing a `MenuBook`. Archived items are omitted; an unavailable placement compiles
  to a **present-but-86'd** entry (the published-availability floor); an unknown item, a missing menu,
  or an inheritance cycle is refused with a named `CompileError`, never substituted. Output is
  deterministic (channels by wire token, entries by item id), so re-compiling unchanged authoring
  yields a byte-identical snapshot and the config tree ships a new version only on real change. This
  is the keystone of Phase 2a — where the authoring model actually becomes the price book the store
  reads. (Wiring it to a store's menu assignment and publishing it to the config tree are later
  slices.)
- **The cloud now has a catalog authoring seam — the source of truth a menu compiles from**
  (ADR-0066, Phase 2a). `pos-cloud` gains a `CatalogStore` trait and its records for the item master,
  menus (with a `parent_menu_id` inheritance edge), and menu placements (an item in a menu, with its
  per-channel prices and a published-availability floor). It is deliberately distinct from the config
  tree (which carries only compiled output) and the registry (identity/naming), mirroring how
  ADR-0065 separated those — tenant-scoped create/list/update, entities archived not deleted, an
  in-memory fake proving the contract. A menu id is a cloud-authoring concept that never crosses the
  wire, so `MenuId` is defined beside the seam like `BrandId`. No Postgres or admin routes yet (later
  slices); this is the model the compiler reads to produce a `MenuBook`. Prices are a T2 asset —
  authored and compiled in the cloud, never logged.
- **The compiled menu now carries a per-channel presentation plan, separate from its prices**
  (ADR-0066, Phase 2a). `pos-proto` gains `DisplayPlan` (display categories → sub-categories →
  item buttons, each with an optional POS grid position) and its channel-keyed `LayoutBook` — the
  layout twin of `MenuBook`, delivered on a separate `layout` config node so a button moving reprices
  nothing and a price change relays no buttons. The display taxonomy (`DisplayCategoryId`/
  `DisplaySubcategoryId`) is deliberately distinct from an item's operational category: a screen can
  group "Summer specials" while those items report under "Pizza". `LayoutBook::plan_for(channel)` is
  total (unconfigured channel → empty plan), and `pos-core` never reads any of it — only the POS /
  tablet / QR / marketplace UI does. Additive and forward-compatible.
- **The compiled menu can now price per sales channel** (ADR-0066, Phase 2a). `pos-proto` gains a
  `MenuBook` — a channel-keyed set of `MenuCatalog`s plus a fallback — so the same item can be one
  price dine-in and another on delivery without touching the tested `MenuEntry`/`reprice_line`
  contract: the cloud resolves the channel at compile time and the edge selects the right catalog
  with `MenuBook::catalog_for(channel)` (total — an unpriced channel gets an empty catalog and refuses
  every line, never guesses). This is the first `pos-proto` shape of the cloud catalog whose design
  ADR-0066 fixed; the compiler that fills it and the edge wiring that reads it land in later slices.
  Additive and forward-compatible (the fallback is `#[serde(default)]`); `MenuCatalog`/`MenuEntry`/
  `TaxRateTable` are unchanged.
- **The new-store wizard now hands the operator a ready-to-run `config.toml`, and a runbook ties store
  provisioning together end to end** (ADR-0065, WS-C #102). The wizard created a store and issued its
  API key but stopped short of the one file the store machine actually needs. Its handoff step now
  generates the edge's bootstrap `config.toml` — the store's `store_id` as the one active key, with the
  store/tenant names and this cloud's URL as comments — and offers **Copy** and **Download config.toml**.
  The file is deliberately minimal: the edge's config schema is `deny_unknown_fields` and carries only
  identity, because a store server gets its credential by activation, never from a file on disk
  (ADR-0004, ADR-0051) — so the generator emits exactly what the binary accepts and nothing that would
  make it refuse to start. A new guide, **[Bring a store online](docs/guides/bring-a-store-online.md)**,
  walks the whole path: create the store → download `config.toml` → install the service → sell offline →
  activate devices → publish configuration, honest about which steps compose into the shipping binary
  today. Also: the device-proposals screen now shows a proposal's store by its **registered name**
  instead of a raw ULID, resolved against the registry. Bilingual EN/VI, behind the no-hardcoded-strings
  lint.
- **Device activation now picks (or adds) a device by name — the last raw-ULID entry point is gone**
  (ADR-0065, WS-C #102). The Activation screen asked the operator to type a `Device slot ULID`, the
  final place in the dashboard that still demanded a hand-typed identifier. It now **loads the store's
  devices from the registry** (`GET /admin/stores/{store_id}/devices`) and lets the operator choose one
  by name and kind, or **add a device** inline (name + a kind picker: POS terminal, printer, kitchen
  display, tablet) over `POST /admin/stores/{store_id}/devices`, before issuing the one-time activation
  code (still shown once). Both cards are scoped to the picker's tenant and store and sit behind the
  super-admin session. Bilingual EN/VI, behind the no-hardcoded-strings lint. This completes WS-C's goal
  of removing every ULID-entry field from the operator-facing dashboard.
- **A guided new-store wizard onboards a store from zero without a ULID** (ADR-0065, WS-C #102). A
  three-step flow at `/stores/new` (linked from the Stores screen): name the store and optionally put
  it under a brand → it is created in the registry → issue the scoped API key its devices use to reach
  the cloud (shown once) → a handoff summary with the store's id and the next steps (activation,
  configuration). It composes the registry and API-key admin routes; the tenant comes from the picker.
  Bilingual EN/VI, behind the no-hardcoded-strings lint. Device activation from inside the flow is the
  next slice.
- **A Stores & brands management screen names the stores the registry backfilled** (ADR-0065, WS-C #102).
  The backfill (migration `0011`) surfaced every existing store under a placeholder name like
  `Store 01J9…`; there was no way to fix that or to add a store from the dashboard. A new **Stores**
  screen (behind the super-admin session, scoped to the picker's tenant) lists the tenant's stores,
  **renames** them, **creates** new stores and brands, **assigns** a store to a brand, and
  **archives / restores** — all by name, over the registry's `/admin/stores` and `/admin/brands`
  routes. Bilingual EN/VI, behind the no-hardcoded-strings lint. The guided create-store **wizard** and
  device naming are the next slices.
- **The dashboard now picks the tenant/store by name, not by typing a ULID** (ADR-0065, WS-C #102).
  The top bar's two free-text `Tenant ID` / `Store ID` boxes — the ones that leaked
  `tenant_id is not a ULID` and made the screen unusable for a non-technical operator — are replaced
  by a **context picker** that reads the new registry (`GET /admin/tenants`, `/admin/stores`) and
  lets the operator choose a tenant, then one of its stores, by name; the ULID sits underneath, muted,
  for reference only. Every screen still reads the same id in context, so nothing downstream changes —
  it is just chosen from a list now instead of typed. Bilingual EN/VI, behind the no-hardcoded-strings
  lint. The create-store wizard and the full CRUD screens are the next slices.
- **The cloud now has an org registry — named Tenant/Brand/Store/Device** (ADR-0065, WS-C #102). The
  cloud has always addressed a store by two opaque ULIDs; nothing recorded that a tenant, brand, or
  store *exists*, what it is *called*, or which brand and tenant a store *belongs to*. A new
  `store-postgres` migration (`0011`) adds `tenants`, `brands`, `stores`, and `devices` tables —
  each a named, status-bearing row with its parentage, RLS-isolated by tenant exactly as
  `config_trees` is — and a `RegistryStore` seam with `/admin/tenants|brands|stores` (and
  `/admin/stores/{id}/devices`) CRUD behind the super-admin session. Identity and naming live here;
  configuration keeps living in the config tree (ADR-0033), and a store shares its `store_id` between
  the two. `devices` is the canonical device identity that `device_proposals`/`device_credentials`
  key to, not a fourth copy. This is the backbone that lets the back-office dashboard (ADR-0060)
  replace free-text ULID entry with named pickers and a create-store flow — so a normal operator
  never sees or types a ULID, and `tenant_id is not a ULID` stops being reachable. **Upgrade note:**
  migration `0011` runs on boot and **backfills** the registry from existing `config_trees` rows —
  every already-configured `(tenant_id, store_id)` becomes a tenant and a store under a placeholder
  name (`Store <short-ulid>`), idempotently, touching nothing in `config_trees`; renaming them is a
  non-blocking follow-up. No config, protocol, or permission identifier changes.
- **The intake idempotency ledger is now durable and written in the order's own transaction**
  (ADR-0064). `IntakeLedger` is promoted to a `pos-ports` port (a `Transactional` one, like
  `ConfigStore`): `record` buffers the `(sales_channel, external_reference) → record` row into the
  caller's transaction, so it and `sales.order.opened` **commit together or not at all** — a crash
  between opening an order and recording it can no longer let a retry open a second one. `store-sqlite`
  implements it against an `intake_ledger` table (migration 0004); the fake implements it in memory;
  both pass the shared `OrderIn` contract suite. The key is a **plain** insert, so a second order
  racing in on the same reference fails its commit with `already_exists` (the one writer thread
  serialises the two) and rolls back rather than duplicating — `EdgeOrderIn` resolves the loss by
  looking the winner up. A store-sqlite test proves a recorded key survives reopening the database
  and that a duplicate key is refused. With this, **nothing in the order-intake path is in-memory
  any more** — both the dedupe ledger and the daily queue counter are durable. `EdgeOrderIn` drops
  its injected ledger generic (`EdgeOrderIn<S, Q>`): the store `S` now supplies both the event log
  and the ledger, which is what lets them share one transaction.
- **The edge implements `OrderIn`** (ADR-0064) — the store side of order intake. `EdgeOrderIn` reprices
  each inbound line from the store's synced menu catalog (ADR-0063), opens a **tableless** order in the
  local event log (`sales.order.opened` + `sales.order_line.added` in one transaction), and dedupes on
  the caller's `(sales_channel, external_reference)` through a per-store intake ledger — so a
  marketplace's retry or the relay's at-least-once delivery converge on one order in the kitchen. The
  acceptance total is the store's own menu total (tax-inclusive); an unknown item is refused
  (`invalid_argument`), never substituted; a QR order (one that names a table) awaits staff
  confirmation while a delivery/public-API order does not; and it accepts **offline**, since the menu
  is local config and the order is a local log write. Proven against the shared `OrderIn` contract
  suite — the same suite `FakeIntake` passes — so "the edge is a real `OrderIn`" is verified, not
  asserted.
- **A tableless order now gets a durable, daily-resetting queue number** (ADR-0064) — the number the
  counter shouts a takeaway customer back by. A `QueueNumberAuthority` (the same in-memory-vs-`SqliteStore`
  split as `ReceiptAuthority`) is injected into `EdgeOrderIn`; the `store-sqlite` implementation
  (migration 0003, a `queue_counter` keyed by `(store, business_date)` plus an order-keyed
  `queue_allocations` idempotency table) **resets each trading day with no midnight job** and is
  **durable** — a store-sqlite test proves the sequence survives reopening the database, so a box that
  lost power mid-service does not reissue `#1` at a second customer. Allocation is idempotent by order,
  and a QR order (one that names a table) still gets none. The intake **ledger’s** durable SQLite
  backing (in the order’s own transaction) remains the one flagged follow-up; the in-memory ledger
  behind the trait + contract suite ships here.
- **The store menu catalog — the store server's authoritative price book** (ADR-0063), the missing
  piece a real edge `OrderIn` needs. The dine-in path prices on the device, but an inbound order
  (marketplace, `POST /v1/orders`, QR) arrives as identifiers and quantities with no device to price
  from, so the `OrderIn` contract's "the store's price wins" and "an unknown item is refused, never
  substituted" have nothing to stand on. A new `MenuCatalog`/`MenuEntry` (in `pos-proto`, alongside
  the other config shapes) is a per-item price book — `unit_price`, `tax_class_id`, `available` — that
  the cloud publishes under the config tree's `menu` node and the store syncs like any other config.
  A pure `pos_core::menu::reprice_line` turns `(menu_item_id, quantity, modifiers)` into a priced line
  (base + modifier prices, extended by quantity, taxed via the existing channel-keyed `TaxRateTable`),
  or refuses it: an unknown item or modifier (`invalid_argument`), an 86'd item (`failed_precondition`),
  or a class with no rate on the channel (`failed_precondition`, never a silent zero). The caller's
  quoted price is compared and flagged `repriced`, never charged. No consumer yet — the edge `OrderIn`
  that reprices from it is the follow-up PR; this lands the price book and the repricing law, both
  property-tested.
- **The cloud→store order relay** (ADR-0061), so `POST /v1/orders` answers in the binary. Inbound
  orders (marketplace, the public API, QR) are held in a durable per-store `order_queue` and the
  store **pulls** them over its own outbound sync channel — no cloud→store push, so "stores dial out
  only" (4G/CGNAT with no port-forward) is unchanged. The cloud implements `OrderIn` over the queue:
  `submit` enqueues idempotently on `(tenant, store, channel, reference)` and parks up to the store's
  configured deadline, returning the store's real acceptance (`201`/`200`) if it arrives or `503`
  on timeout **with the order still queued**; `look_up` (`GET /v1/orders`) resolves a timed-out
  caller. The store pulls (`GET /sync/stores/{id}/orders`, a bounded long-poll) and reports outcomes
  (`POST /sync/stores/{id}/orders/{queued_id}/ack`) under a new deny-by-default `relay_orders` scope.
  Per-store `store.order_relay.{enabled,wait_ms}` is published from the cloud through the config tree,
  so intake is toggled and tuned per store from the dashboard with no deploy. Validated against the
  in-memory fakes (queue → pull → ack → look-up, idempotency, scope enforcement); `store-postgres`'s
  `order_queue` table is proven by its own integration suite.
- **A back-office dashboard, embedded in `pos_cloud` and served at `/`** (ADR-0060). A new
  `dashboard/` SolidJS + Tailwind SPA — built like the edge `ui/` (shared design tokens, the ICU
  i18n runtime with `en` the enforced floor and a `vi` catalogue, a typed client, the
  no-hardcoded-strings lint) — gives the cloud its first screen. `pos_cloud` embeds it with
  `rust-embed` and serves it as the router's fallback (`crate::assets`), so the API routes match
  first and everything else resolves to the SPA; a `build.rs` writes a placeholder on a fresh
  checkout, exactly as `pos_edge` does. Screens cover the existing `/admin` surface: super-admin
  login + mandatory TOTP and first-run setup, the **config-tree publish editor** (publish a
  tenant/brand/store/device level and view the effective config — the operator half of config
  delivery), API-key provisioning, the printer/KDS approval queue, the translation grid, webhook
  registration, activation codes, and a daily-rollup report. Auth is the existing super-admin
  session cookie unchanged; the SPA holds no secret. A `dashboard` CI job type-checks, lints and
  builds it; `deploy/Dockerfile` gains a Node stage that builds `dashboard/dist` natively (no
  emulation) and embeds it. (ADR-0060)
- `GET /admin/stores/{store_id}/rollups/daily` — the daily rollup read under the super-admin
  session, naming the tenant with `?tenant_id=` (ADR-0060). The `/v1` rollup read stays bearer-authed
  and tenant-scoped by the key; this admin read reuses the same `RollupStore` for the dashboard,
  following the same admin-is-global pattern the config read already uses. Read-only.
- The specification set is now in the repository: `docs/`, ADRs 0001–0012, `AGENTS.md`,
  `CONTRIBUTING.md`, `SECURITY.md`, `MAINTAINERS.md`, `CODEOWNERS`, the GitHub templates,
  and the frozen Vietnamese design archive.
- `LICENSE` — proprietary, internal use, as decided in ADR-0009. It was referenced by
  `README.md` and the ADR but had never been written.
- `docs/roadmap.md` — the dependency-ordered build plan from an empty repository to a pilot
  store, with an exit criterion per phase and no calendar dates.
- ADR-0013 (sans-I/O domain core, async ports, static dispatch), ADR-0014 (date, time and
  timezone library), ADR-0021 (the sixteen ports, superseding 0006), ADR-0024
  (`PROTOCOL_VERSION` negotiation), ADR-0026 (port shapes: one `PortError`,
  `Transactional`/`TxContext`, outbox cursor ordering, fault injection on the harness, and
  three corrections to ADR-0013).
- `pos-ports`: all sixteen ports from ADR-0021. `PortError` carries an AIP-193 status and the
  `PortName` that produced it, so retry policy and the error mailbox need no per-port
  translation. `Transactional` is a supertrait of `EventStore` and `ConfigStore`, so an
  adapter implementing both has exactly one transaction type and "the outbox row commits with
  the state change" is the only thing that type-checks. Object-safe mirrors
  (`DynPrinterDriver` and four others) cover the families selected by configuration rather
  than at compile time.
- `pos-contract-tests`: a shared suite per port, 104 cases in all, parameterised by a harness the
  adapter supplies. Suites are emitted by a macro that takes the adapter's own `block_on`, so this
  crate depends on no async runtime. Destructive operations — losing power, severing a link, staging
  an ambiguous card result — live on the harness rather than on the ports, so no production adapter
  ships a way to corrupt itself. A test asserts every port has a suite.
- `pos-fakes`: in-memory implementations of all sixteen ports, with fixed-capacity queues returning
  `RESOURCE_EXHAUSTED`, and `tests/contract.rs` running every suite against them. `pos-core`'s tests
  will run against these fakes, so this is what stops the domain suite resting on an unchecked
  assumption about how the real store behaves.
- ADR-0027 and `pos-country`: a country module is a **bundle** — a `Fiscalization` implementation, a
  locale pack, tax-code format validation, a default retention period — living at `countries/<cc>/`
  rather than filed among the adapters. Selection is a Cargo feature per country, so a fork serving
  one country edits one line, deletes nothing, and compiles nothing else. `pos-proto` gained
  `CountryCode`, `TaxRate` in basis points, the channel-keyed `TaxRateTable` and `LocalePack`.
- `countries/zz/`: a reference country module that **passes the whole `Fiscalization` contract
  suite**, so a real country fills in a proven shape. `cargo xtask countries` fails a module that is
  misnamed, absent from the workspace, or wired into no binary's features.

- ADR-0025 (receipt-number authority is configuration: gapless only while one store authority is
  reachable), ADR-0028 (the real settlement invariant — `sum(applied) == total_due`, tips a separate
  ledger, cash rounding as an explicit line, tax per tax-class subtotal, service-charge taxability as
  config), and ADR-0029 (line merge: terminal states win, other fields last-writer-wins on
  `(event_time, device_id)`, commutative and associative). These resolve issues #8, #9 and #10 ahead
  of the P3 domain code they govern; `pos-spec.md` §3/§5/§14 and `architecture.md` §2/§5 are amended
  to state the rules the code will enforce.

- `pos-core` begins: the state-machine framework and the five lifecycles from `architecture.md` §5
  — Table, Order, order line, Bill, Shift. Transitions are data, enumerable at runtime, so
  `docs/state-machines.md` is generated from the code (a test keeps it in sync) and one generic
  checker proves every machine exhaustively: no reachable undefined state, no orphan or deadlocked
  state, terminal states with no exit, and a merge that is commutative, associative and
  terminal-preserving. That merge is the ADR-0029 rule — `VOIDED` outranks every editable state, so
  a concurrent edit can never resurrect a voided line — and `bill:settle` being one-time (§14.4) is
  now a property of the `Bill` machine rather than of a lock.

- `pos-core` billing: `assemble` computes a bill's totals with tax **per tax-class subtotal**
  (rounded once per class, the per-class lines reconciling to the tax total a VAT invoice prints),
  bill-level discounts and comps allocated proportionally across classes, a service charge that is
  taxable per config, and cash rounding materialised as an explicit `rounding_adjustment` line —
  ADR-0028 made mechanical. `settle` proves the settlement invariant (`sum(applied) == total_due`,
  change `= tendered − applied − tips`, tips a separate ledger) and refuses under-application and
  negative change. `split_evenly`/`split_by_weights` bind the §14.3 law (`sum(splits) ==
  original_total`) at the domain level. A `DomainError` names which rule the inputs broke. Property
  tests over arbitrary amounts, parts, weights and payments bind §14.3 and §14.5 in CI.

- `printer-escpos` (P4): the ESC/POS thermal-printer adapter for the `PrinterDriver` port. It encodes
  a `PrintDocument` into ESC/POS bytes (text with emphasis/size/alignment, raster bitmaps, CODE39
  barcodes, QR codes, feed, cut) and pushes them at a `Transport`; it does not decide text-vs-bitmap
  (the framework already did, from the code page this adapter reports — ADR-0026 §5). It is idempotent
  by `job_id` (a flaky cable's retry prints one ticket), refuses to open a drawer on anything but USB
  (port 9100 has no authentication), returns `unavailable` when unreachable so the caller re-queues,
  and `failed_precondition` when out of paper. Passes all **8** `PrinterDriver` contract cases via an
  in-memory recording transport. The retry queue and backup-printer failover live at the caller (port
  §2), and the real USB/serial/TCP transports plus the real-print test land with A5 hardware.

- `cargo xtask migrations` (P4, ADR-0017 enforcement): the additive-only gate. It refuses a pull
  request that edits a migration already shipped on the base branch (a migration is immutable — the
  same removal-gate mechanism `xtask snapshot` uses) or that adds a destructive statement
  (`DROP TABLE`, `DROP COLUMN`, `RENAME`) without the reviewed `-- migrations:allow-destructive`
  marker. Wired into the PR workflow and the `just migrations-check` recipe; the destructive
  detection is unit-tested, and the shared git-diff helpers are factored into `xtask::checks` so the
  snapshot and migration gates share one implementation.

- `store-sqlite` (P4): the edge `EventStore` and `ConfigStore` over SQLite, the first adapter. One
  `rusqlite` connection owned by a dedicated writer thread (ADR-0015); the async port methods send a
  command over a bounded channel and await a oneshot reply, so blocking SQLite never touches the
  executor and every write serialises through one point. The outbox position is an AUTOINCREMENT
  rowid assigned inside the commit transaction — monotone, never reused after an acknowledged delete,
  and starting at one so it never collides with `OutboxPosition::START`. A transaction buffers its
  writes in the `SqliteTx` and flushes them in one `BEGIN IMMEDIATE`…`COMMIT` on commit, so a dropped
  handle rolls back and a crash mid-transaction loses only the uncommitted work. Idempotency is
  `INSERT OR IGNORE` (the stored copy wins), reads come back ordered by `event_id`, and the schema is
  WAL with `synchronous = NORMAL`. It passes **all 19** shared contract cases — the same suites the
  fake runs — including both power-loss cases, driven by reopening the database file. The schema is
  the forward-only migration `0001_event_store.sql` applied by the ADR-0017 runner.

- ADR-0015 (SQLite access at the edge: `rusqlite` behind one dedicated single-writer thread, bridging
  blocking SQLite into the async `EventStore`/`ConfigStore` ports over a channel, so gapless outbox
  positioning and `TxContext`-by-shape fall out for free) and ADR-0017 (migrations: forward-only,
  additive, numbered SQL files with a tiny runner and a `cargo xtask migrations` gate that refuses
  editing a shipped migration or a destructive statement). Both block P4 and are merged ahead of the
  `store-sqlite` code they govern.

- ADR-0018 (edge HTTP/WebSocket stack): `pos_edge` serves its UI-facing HTTP over **axum** on
  `hyper` + `tower`, opens WebSockets with axum's built-in `ws` extractor, and fans state changes out
  to every device on the store LAN over a single bounded `tokio::sync::broadcast` channel — an
  in-process send, so the under-50 ms budget is met by construction and a stalled device degrades
  itself (a `Lagged` resync) rather than growing server memory. The SolidJS UI is compiled into the
  binary with `rust-embed` so the store is one static file (a `dev-ui` feature reads from disk
  instead); tokio, axum, tower and rust-embed enter at the binary layer only, and the dependency-rule
  test keeps them out of `pos-core`. This governs the edge's internal transport only — the cloud's
  public `/v1` API and its OpenAPI are ADR-0019 (P7). Merged ahead of the P5 `pos-edge` code.

- ADR-0030 (edge discovery, pairing, offline auth): the always-works discovery path is a QR code
  carrying a raw-IP URL plus manual `IP:port` entry (no name resolution to fail on), with a DHCP
  reservation pinning the IP; mDNS `pos.local` is a convenience behind an `Advertiser` trait whose
  real multicast implementation lands with hardware (like the printer's transports), so no mDNS
  dependency enters the framework now. Device pairing is a single-use, five-minute 6-digit code from a
  vetted CSPRNG. User authentication is a PIN verified offline against cloud-synced Argon2id hashes,
  with a five-failure/five-minute lockout enforced locally in `pos_edge` — the Argon2id cost plus the
  lockout, not PIN entropy, is the brute-force defence. Device tokens and employee ids are logged;
  PINs, hashes, and pairing codes never are. Adds `getrandom` and `argon2` at the binary layer only.
  Merged ahead of the P5 code it governs.
- `pos-edge` (P5): the store binary begins, as a library plus a thin `main` so the HTTP surface is
  testable without binding a socket. It boots an **axum** server (ADR-0018) that answers a `/healthz`
  probe — status, version, protocol version, store id, and no PII — and serves the operator UI, which
  is compiled into the binary with `rust-embed` so the store is one static file (a `dev-ui` feature
  reads `ui/dist` from disk instead). An unknown path falls back to `index.html` for the P6
  single-page app rather than 404ing. Bootstrap config is TOML with `deny_unknown_fields` and a
  required `store_id`; `tracing` is configured in one place with the no-PII rule stated; the server
  drains in-flight requests on Ctrl-C or `SIGTERM`. `ui/dist` is gitignored build output, so a
  `build.rs` writes a placeholder `index.html` there when P6 has not yet built the real one (and
  never overwrites a real build). The binary wires the reference country module (`country-zz`,
  ADR-0027) as a Cargo feature and validates the registry at start-up, logging which countries it can
  serve. The async runtime and HTTP stack enter at the binary layer only — `cargo xtask deps-rule`
  proves they never reach `pos-core`.
- `pos-edge` WebSocket fan-out (P5, ADR-0018): a `/ws` endpoint gives each device one socket fed by a
  single bounded `tokio::sync::broadcast` channel. When the edge applies a change it publishes once
  and every device receives it — an in-process send, so the under-50 ms LAN budget is met by
  construction. The channel is bounded (`FANOUT_CAPACITY`): a device that falls behind is told to
  reload a fresh snapshot (`ServerMessage::Resync`) rather than making the server buffer without
  limit — the same bounded-memory discipline the SQLite writer uses. `ServerMessage` is
  `#[non_exhaustive]` with an internal `type` tag, so a client dispatches on one field and tolerates
  message kinds added later. An integration test binds a real port and proves a published event
  reaches one connected device, and that two devices on one table both receive the same change.
- `store-sqlite` gapless receipt numbers (P5, ADR-0025): the `store_server` authority. A new
  additive migration (`0002_receipt_counter.sql`) adds a per-store counter and a per-bill allocation
  table, and `SqliteStore::allocate_receipt_number` hands out the next number in one `IMMEDIATE`
  transaction. Because every allocation funnels through the one writer thread, the sequence is
  gapless and collision-free even when two cashier devices settle at once; a test drives 200
  concurrent allocations and asserts the result is exactly `1..=200`. Allocation is idempotent by
  `bill_id` — a retry after a crash reuses the number rather than skipping one — and survives
  reopening the database. This is the store's receipt number, never a legal invoice number (the
  country module's, from a pre-allocated range); the two are deliberately never conflated.
- `pos-edge` offline PIN authentication (P5, ADR-0030): `auth::verify_pin` checks a PIN against a
  cloud-synced Argon2id PHC hash with no network, and `auth::Lockout` is the five-failure/five-minute
  lockout — a pure state machine over `(employee, verified, now)`, so the window is unit-tested in
  microseconds against a fixed clock rather than by waiting. A correct PIN while locked out is still
  refused (the lockout must be served); the window lifts and the count resets after five minutes; a
  malformed stored hash is never a way in. PINs and hashes are secrets and never logged; only the
  employee id and the outcome are. Adds `argon2` at the binary layer, and a dated `rand_core@0.6`
  deny skip for the salt-generation line argon2 pulls but the edge (verify-only) never uses.
- `pos-edge` device pairing and discovery (P5, ADR-0030): the edge mints a single-use, five-minute
  6-digit code from the OS CSPRNG (`getrandom`) and shows the operator a raw-IP pairing URL
  (`http://<ip>:<port>/pair?code=NNNNNN`) — the discovery path that needs no name resolution; a device
  redeems the code at `POST /api/pair` for a 128-bit device token. mDNS `pos.local` is a convenience
  behind an `Advertiser` trait whose real multicast implementation lands with hardware (a
  `NoopAdvertiser` default ships now, like the printer's placeholder transports). `SystemClock` is the
  edge's one sanctioned reader of the OS clock (the single place `clippy.toml`'s `SystemTime::now` ban
  is lifted); everything time-related, including pairing-code expiry, reads it through `ClockSource`,
  so it is testable against a fixed instant. Config gains an optional `advertised_ip` (the
  DHCP-pinned LAN IP) for the pairing URL. Pairing codes and device tokens are secrets and never
  logged.
- `pos-edge` ULID `IdGenerator` and SNTP drift monitor (P5): `idgen::EdgeIdGenerator` mints
  monotonic, time-sortable ULIDs over a `ClockSource` — it clamps to a non-decreasing timestamp so an
  NTP step backwards cannot emit an id that sorts before one already handed out (the event feed pages
  by ULID), and increments the random component within a millisecond so same-ms ids strictly
  increase. The 80 random bits come from a SplitMix64 stream seeded once from the OS CSPRNG; a ULID's
  randomness is not a secret (pairing codes and device tokens take OS entropy directly). `sntp::assess`
  is the pure drift decision — an offset past two seconds from a reference clock alarms, because the
  business date is derived from the store's local time and a drifting clock files sales under the
  wrong day. The SNTP network poll that feeds it lands with deployment, like mDNS.
- `pos-edge` config hot-reload and service units (P5): `active_config::ActiveConfig` swaps the running
  configuration atomically and in well under a second, retaining the previous good version so a
  change that turns out wrong rolls back one step, and refusing a candidate that fails validation
  without touching the active config — a bad config cannot brick the store. Reads take a short read
  lock and clone an `Arc`, so a handler reading config to answer a screen never blocks on a writer;
  content validation against the config schema is generic (the schema is P7). `deploy/edge/` adds a
  hardened systemd unit and a Windows service guide; both deliver the `SIGTERM`/stop the binary
  already drains gracefully, so a committed sale is durable and an interrupted one was never
  acknowledged.
- `pos-edge` application layer (P5 keystone): `app::Edge<S>` is the load → decide → apply → publish
  loop ADR-0013 gives each binary. For a command it loads the aggregate's state from an in-memory
  projection, decides with the synchronous `pos-core` spine, writes the wire events it maps to inside
  one store transaction, and — only after the commit — folds the change into the projection and
  publishes it to every device over the fan-out, so a rolled-back write is never shown. It is generic
  over the store `S`, so the identical loop runs against `pos-fakes` in a test and `store-sqlite` on a
  real machine (static dispatch, no `dyn`). This slice wires the table floor cycle (seat →
  `sales.table.opened`, clean → `sales.table.closed`); the order, bill and shift families follow the
  same shape. Tests prove seating opens a table, that two devices both see the change over the
  fan-out (the dine-in exit criterion in miniature), and that an illegal transition is refused and
  publishes nothing. `StoreIdentity` and `EdgeSession` carry the envelope context and the
  config-driven decision inputs.
- `pos-edge` HTTP domain routes (P5): the table floor cycle is now reachable over HTTP —
  `POST /api/tables/{id}/seat`, `POST /api/tables/{id}/clean`, `GET /api/tables/{id}` — each a thin
  shell over the application loop that returns the table on success, `409 Conflict` for an illegal
  transition (the caller's fault, not the server's), and `400` for a non-ULID id. `serve` is now
  generic over the store and composes the domain router with the infra router, **sharing one fan-out**
  so a committed change over HTTP reaches every `/ws` device. The real `pos-edge` binary composes
  `store-sqlite` (with a `store_path` config key), and `examples/minimal-edge` composes `pos-fakes`;
  `StoreIdentity::for_store` and `EdgeSession::bootstrap` supply the envelope context and
  config-driven decision inputs until the cloud config tree (P7). An integration test drives seat →
  read → clean and the 409/400 paths without a socket. The acting actor is a fixed development
  identity pending token→actor resolution.
- `pos-edge` order-line flow (P5): `Edge::add_line` records a line on the order a table holds
  (`sales.order_line.added`) and `Edge::fire_line` sends it to the kitchen
  (`sales.order_line.fired`) through the `pos-core` `decide_line` spine, consuming its recipe (§8).
  The edge does not invent prices: a `LineDraft` carries the amounts the device captured from the
  menu it holds (`unit_price`, `line_total`, `tax_class`, `tax_rate` — a line never references the
  live menu), and the projection remembers each line's item, quantity and state so a fire can be
  decided and (once the menu's bill of materials syncs, P7) its consumption computed. Firing an
  already-fired line is refused by the state machine; adding to an unseated table is refused. The
  `commit_and_publish` half of the loop is now generic, shared by every command. The bootstrap
  session carries an empty `RecipeBook` (an unrecipe'd item consumes nothing) until P7.
- `pos-edge` bill flow (P5, ADR-0025/ADR-0028): `Edge::open_bill` opens a bill on the order a table
  holds (`billing.bill.opened`) and moves the table to awaiting payment; `Edge::settle_bill`
  assembles what is owed from the order's captured line totals (`billing::assemble`, tax per
  tax-class subtotal), proves the payments sum **exactly** to it (`decide_bill` → `billing::settle`),
  allocates the gapless per-store receipt number for that bill, then appends
  `billing.bill.settled` carrying the number and the subtotal/reduction/service-charge/tax/rounding
  breakdown, and cycles the table to needs-cleaning. A split tender (cash + card) that sums to the
  total settles; an underpayment, a second settle, and a bill on an unseated table are each refused.
  The `Effect::PrintReceipt` is returned on the `BillView` for the caller to run after commit — the
  edge holds no printer, so a rolled-back settle prints nothing.
- `receipt::ReceiptAuthority` (P5, ADR-0025): the gapless receipt-number authority is injected into
  the generic `Edge<S>` rather than derived from its store type. The real binary passes the
  `SqliteStore` itself (its single writer thread is the authority); `receipt::InMemoryReceipts` is the
  same gapless, bill-idempotent contract without a database, for the example and the engine tests.
  This is the store's receipt number, never a legal invoice number.
- `EdgeSession` now carries the store's channel-keyed `TaxRateTable` and default `SalesChannel` (D6),
  so a bill assembles a real total offline. The bootstrap rates one standard class
  (`EdgeSession::standard_tax_class`) at 10% dine-in until the cloud config tree (P7) supplies the
  menu's classes; Vietnam v1's single rate is a special case of the same two-dimensional table.
- `pos-edge` cash-shift flow (P5, §6/§11.1): `Edge::open_shift` opens a shift with a starting float
  (`cash.shift.opened`), `Edge::count_shift` records the **blind** count (`cash.shift.counted`) —
  returning no expectation or variance, so the cashier counts before the system reveals what it
  expected — and `Edge::close_shift` reveals the expected drawer cash (opening float plus the cash
  its bills took) and the variance (`cash.shift.closed`), surfacing `Effect::PrintShiftReport` for
  the caller. Only cash tenders roll into the expectation; card sales, tips and cash rounding never
  touch the drawer. One shift is open per device: a second open is refused, and every event minted
  while a shift is open now carries its `shift_id`. A close that skips the count is refused by the
  state machine.
- `pos-edge` order, bill and shift routes over HTTP (P5): the whole sell cycle is now reachable —
  `POST /api/tables/{id}/lines` and `POST /api/lines/{id}/fire` (order), `POST /api/tables/{id}/bill`
  and `POST /api/bills/{id}/settle` (bill), and `POST /api/shifts`, `POST /api/shifts/{id}/count`,
  `POST /api/shifts/{id}/close` (shift). Each is a thin shell over the application loop, sharing one
  error mapper (a refused command is `409`, an unreachable store `503`, a non-ULID id or unknown
  payment method `400`). A payment method arrives as an `Open` enum, so an unrecognised token is a
  clean `400` rather than a deserialise failure, and the domain boundary refuses an unspecified one.
  An integration test drives a table seat → line → fire → open bill → settle (gapless receipt) →
  clean, and a shift open → blind count → close, entirely over the router without a socket.
- `pos-edge` records each tender as a `billing.payment.captured` event (P5): a settle now appends one
  captured-payment event per tender **and** the `billing.bill.settled` event in a single transaction,
  so a crash never leaves a receipt without its payments. The captured payments are what let the
  shift cash roll-up be rebuilt from the log; a cash payment's outcome is `CAPTURED`, tips are held
  apart (per-payment tip capture is P7).
- `pos-edge` rebuilds the projection from the durable log at boot (P5 crash recovery, ADR-0015):
  `Edge::rebuild` replays every event in `event_id` order and folds it back — table, order line,
  bill, table cycle, and the shift float-plus-cash roll-up — so a restart resumes exactly where the
  last committed transaction left off and only an *uncommitted* transaction is lost. The `pos-edge`
  binary calls it before serving. Idempotent: replaying committed facts lands on the same state.
  Integration tests prove a second edge over the same store recovers a settled sale, a fired line,
  the cleaned-down table cycle and the shift's cash total, and that a double rebuild is a no-op.
- The **dine-in acceptance flow** is now an automated test (`tests/dine_in.rs`, the P5 exit
  criterion): one table, two devices, no network — seat → both devices order → fire by course → add
  a later course → open the bill → settle it split across cash and card → a gapless receipt → the
  table cycles to clean. Every committed change reaches both devices over the fan-out, and the flow
  runs entirely on the in-memory fakes, which is the offline demonstration. With this, P5 (`pos_edge`)
  meets its exit criteria: a store seats, orders from two devices, fires, settles with a gapless
  receipt and cycles the table, offline throughout, and a kill mid-sale loses only the uncommitted
  transaction (`tests/recovery.rs`).
- `ui/` (P6) — the operator interface begins: a SolidJS + Tailwind app built with Vite and
  TypeScript, embedded into `pos_edge` with rust-embed (ADR-0018). It carries the design-token file
  (spacing, type, touch, radius, motion and colour, in light and dark), integer-minor-unit money
  formatting, a typed client for the edge's routes, and a reconnecting `/ws` live link that folds the
  fan-out into a small client projection — so what one device does appears on every other. The
  primary flow is playable: a floor plan, a table's order (add items, fire), and a pay screen that
  settles for a gapless receipt with the VND quick-cash denominations; a persistent status bar shows
  the store link (offline from the cloud is a normal working state) and the shift. The remaining
  screens (KDS, expo, Today, shift, pairing) and the four device layouts follow. A CI `ui` job
  type-checks and builds the app on every pull request; the Rust build still embeds `build.rs`'s
  placeholder on a fresh checkout, since `ui/dist` is gitignored. Requires Node ≥ 22 and `pnpm`.
- `ui/` (P6) — the remaining operator screens, over the same live projection: the **kitchen display**
  (every fired line, bump to clear) and the **pass** (fired lines gathered by table, "all away"), both
  on a dark theme they take over while open (legible at two metres); **Today** (the floor at a glance
  — table counts, open bills, shift, a live read rather than a report); the **cash shift** (open →
  blind count → close revealing the variance, the count never shown beside the expectation); and
  **pairing** (redeem a six-digit code, pre-filled from the QR link). A status-bar nav links them.
  Layouts are responsive across phone, tablet and POS from one breakpoint set, with the kitchen and
  pass on their own dark treatment. Known follow-ups, called out rather than hidden: the KDS/pass
  bump is a screen-local acknowledgement until a durable "line made" event exists; ICU i18n with an
  `en` fallback and the no-hardcoded-strings CI check are blocked on ADR-0020; the WCAG-AA audit and
  the per-device layout tuning are part of the visual pass.
- ADR-0020 and `ui/` i18n (P6): the interface is internationalised. Messages are ICU MessageFormat in
  per-locale JSON catalogues (`en.json` canonical, `vi.json` a first-pass Vietnamese translation),
  formatted by `intl-messageformat` over the platform `Intl` (no bundled CLDR data — the embedded
  Chromium already carries it), with **`en` the enforced fallback** so a missing translation shows
  English, never a blank. `t(key, args)` reads a reactive locale signal (a language toggle sits in
  the status bar), and `MessageKey` makes a mistyped key a compile error. **No user-visible string is
  hardcoded**: `pnpm i18n:lint` parses every `.tsx` with the TypeScript compiler and fails the build
  on a JSX text node with a letter, or a hardcoded `placeholder`/`title`/`aria-label`/`alt` — proven
  to fire on a probe. The `ui` CI job runs it, so the seventh standing rule (`AGENTS.md` §2) is now a
  merge gate for the UI. Accessibility: native focusable controls with a visible focus ring, no
  meaning by colour alone, `role="alert"` on errors, the document `lang` tracking the locale, and
  ≥48px touch targets; the numeric WCAG-AA contrast audit of the oklch palette is the remaining
  visual-pass item. Adds `intl-messageformat` to the UI's dependencies (the Rust backbone is
  untouched).
- The four P7 decisions, ahead of the cloud code they govern (ADR-before-code): **ADR-0016** — cloud
  PostgreSQL access is `tokio-postgres` behind a `deadpool` pool with hand-written SQL and RLS set per
  transaction, chosen so the workspace builds with no database and correctness is proven by tests
  against a real PostgreSQL rather than by the compiler. **ADR-0022** — the events table is
  range-partitioned **monthly by `business_date`**, tenant isolation is RLS (a column and a policy, not
  the partition key), and retention drops whole partitions; resolves the three-way partition ambiguity
  and supersedes ADR-0008's "by `store_id`" phrasing. **ADR-0023** — tenants are flat per-tenant
  subdomains with no country label, DNS created through the Cloudflare API is the slug-uniqueness
  ledger (no shared cross-cell database), redirect never proxy, wildcard renewals staggered above ~5
  cells; resolves the ADR-0011/archive contradiction and supersedes ADR-0011's country-in-hostname
  mechanism while keeping its redirect principle. **ADR-0019** — the public `/v1` OpenAPI is generated
  from the axum handlers with `utoipa` and a drift gate (a `pos-cloud` test that renders
  `docs/openapi.json` and fails CI on any difference, the same idiom as the event-catalogue snapshot),
  never hand-written. Registered in the ADR index and the engineering guide; these unblock the P7
  schema, adapters and `pos_cloud`.
- `store-postgres` (P7): the cloud `EventStore` over PostgreSQL, and the second real implementation
  of that port — it passes the **same** shared contract suite as `store-sqlite` and the in-memory
  fake, which is what makes "the cloud store behaves like the edge store" a checked fact. Migration
  `0001_cloud_events.sql`: the event log is range-partitioned **monthly on `business_date`**
  (ADR-0022) with a default safety-net partition and a `create_events_partition` function the cloud
  calls ahead of need, idempotent by `(business_date, event_id)` — the partition key must be in the
  primary key, and a replay carries the same business date, so this is `event_id` idempotency in
  practice. Tenant isolation is row-level security on `tenant_id`: a session that has not set
  `app.tenant_id` sees nothing (default-deny), so a query that forgets its tenant returns empty
  rather than leaking across tenants. The envelope is a `json` column, not `jsonb`, because the
  contract requires a replayed event to read back byte-for-byte identical and `jsonb` reorders keys
  and reformats whitespace. Access is `tokio-postgres` behind a `deadpool` pool (ADR-0016) with no
  build-time database; the pool recycles connections with `ROLLBACK`, which is what makes a
  transaction dropped without commit (the simulated crash) leave nothing behind instead of leaking
  into the next caller.
- The merge-to-`main` `integration` job now runs `store-postgres` against a real `postgres:16`
  (pinned by digest) — the twelve `EventStore` contract cases plus cloud-only tests for RLS isolation
  and monthly partition routing. These live behind the crate's `integration` Cargo feature, so the
  ten-minute pull-request gate neither compiles nor runs them and stays database-free; `just
  test-integration` runs them locally against your own PostgreSQL. `deny.toml` gains three reasoned,
  dated skips for the transient version duplications the `tokio-postgres` stack brings in (the rand
  0.10 line for its SCRAM nonce, and `fallible-iterator` mid-migration).
- ADR-0031 — cloud adapter transports: `async-nats` for `link-nats` (the JetStream client protocol is
  the "genuinely hard and general" infrastructure ADR-0007 says to buy); hand-rolled SigV4+HTTP for
  `blob-garage` (thin and scheduled for deletion once WAL shipping is in-house, so no S3 SDK); a
  bounded-queue HTTP importer for `metrics-vm` (off the sales path, so `record` never blocks).
  Registered in the ADR index and the engineering guide.
- `metrics-vm` (P7): the cloud `MetricsSink` over `VictoriaMetrics`. `record` enqueues into a bounded
  in-memory queue and returns without waiting; a background task flushes batches through a
  transport, so a slow or dead metrics backend drops samples rather than blocking a sale (ADR-0026
  contract 1). No floating point — a sample is an `i64` and a unit, and the unit rides across as a
  label. The transport is hand-rolled HTTP/1.1 over `tokio` to `VictoriaMetrics`' JSON line import
  (`/api/v1/import`), no client crate (ADR-0031). Because the port's contract is this adapter's
  queueing rather than `VictoriaMetrics`' storage, its shared contract suite runs **in process**
  against a capturing transport in the ordinary `test` job, and a separate in-process HTTP mock pins
  the exact import bytes — no live `VictoriaMetrics` needed to verify it. Adds no new external
  dependencies (`tokio` and `serde_json` were already in the tree).
- `blob-garage` (P7): the cloud `BlobStore` over Garage / S3. Thin and temporary by design — object
  storage exists only for Litestream and the port is deleted once WAL shipping is in-house (ADR-0007)
  — so rather than an S3 SDK it hand-rolls SigV4 signing and HTTP/1.1 over `tokio` (ADR-0031),
  path-style, plain `http://`. `put`/`get`/`delete` are idempotent (a repeated put overwrites, an
  absent get is `Ok(None)`, a repeated delete succeeds), and `list` is segment-aware: S3's `prefix`
  is a string match that also returns `stores/10` for `stores/1`, so the adapter filters the result
  through `BlobKey::is_under`, which is what keeps one tenant's listing out of another's. Verified
  three ways: the SigV4 signer's arithmetic against AWS's published `get-vanilla` vector (no server),
  the full contract suite against an in-process S3 mock (request/response framing and the prefix
  filter, in the ordinary `test` job), and — behind the `integration` feature — the same suite
  against a real MinIO in the merge-to-`main` job. Signing uses `hmac`/`sha2` pinned to the
  RustCrypto line already in the tree, so no new duplicate version is introduced.
- `link-nats` (P7): the store→cloud `MessageLink` over NATS JetStream, on `async-nats` — the one
  cloud adapter that carries a real client dependency, because the JetStream protocol is the hard,
  general infrastructure ADR-0007 says to buy (ADR-0031). Outbound only and at-least-once: no
  transaction across NATS and the edge database, so the outbox makes a crash between commit and
  publish safe. The handshake is local — reachability, stream existence, and `pos_proto`'s
  `negotiate` — so the link stays one-directional with no cloud responder. Back-pressure is visible:
  the stream is `discard: new`, a full stream returns `resource_exhausted` (retryable, so the outbox
  holds), and `capacity` reports the fill level the 80% alert reads. Verified against a **real NATS
  server with JetStream** (behind the `integration` feature, wired into the merge-to-`main` job as a
  `docker run -js` step) — all six `MessageLink` contract cases including the severed-link and
  full-stream obligations. `async-nats` is pinned to 0.50 (its `rustls-webpki` is on the patched
  0.103 line; 0.38's 0.102 carried fresh RUSTSEC advisories); its `webpki-roots` (Mozilla CA bundle,
  `CDLA-Permissive-2.0`) is a scoped, reviewed `deny.toml` licence exception, and a
  `skip-tree = async-nats` collapses the transient version straddles its stack introduces.
- `pos-cloud` (P7, first slice): the cloud binary, and its ingest→rollup spine. `Cloud::ingest`
  stores a batch idempotently in one transaction — a replay adds `duplicates`, not `appended`, and
  grows the log by nothing (ADR-0026 §4) — and `Cloud::daily_rollups` folds the log into per-store,
  per-trading-day activity counts (the read model dashboards will answer from, `docs/roadmap.md`
  P7). Both are generic over the `EventStore`, so the same code runs against `pos-fakes` in tests
  and `store-postgres` in the cloud (ADR-0026); the spine is verified against the fake with no
  database. The binary loads config, opens and migrates the PostgreSQL store, and serves an axum
  router (`/health` and `/internal/ingest`, the reconciliation re-push target). Deliberately later,
  each its own slice: the public `/v1` API and generated OpenAPI (ADR-0019), the NATS cursor
  consumer that drives ingest in production, webhooks, super-admin auth (Argon2 + TOTP), the
  four-level config tree, the retention/PII-masking cron, and the dashboard screens with
  materialised rollups.
- `pos-cloud` (P7): the public `/v1` read API and its **generated** OpenAPI (ADR-0019).
  `GET /v1/stores/{store_id}/rollups/daily` returns a store's per-trading-day activity rollups; a
  malformed store id is a `400`, an unreachable store a `503`. The OpenAPI document at
  `GET /v1/openapi.json` is generated from the handlers (`utoipa::path`) and their response types
  (`utoipa::ToSchema` beside the `serde` derives), never hand-written, and is committed at
  `docs/openapi.json`. A `pos-cloud` test renders that file and fails CI on any drift — the same
  opt-in idiom (`POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-cloud openapi`) as `pos-proto`'s
  event-catalogue snapshot, so the document can never disagree with the code. `/internal/*` stays out
  of the external contract by construction.
- `pos-cloud` (P7): the **NATS cursor** — the production ingest feed. `link-nats` gains a
  `NatsConsumer`, the read counterpart of `NatsLink`: a durable JetStream pull consumer whose delivery
  position lives server-side (the "cursor over the event log" a later slice resets to replay), which
  hands the caller each decoded batch *with* its message handles and acknowledges only after the
  caller has stored it — at-least-once with exactly-once effect, given idempotent ingest. A frame
  that is not a valid envelope can never be ingested, so it is terminated and counted (loudly, never
  silently) rather than wedging the cursor. `pos-cloud`'s `cursor` loop drives `Cloud::ingest` from it
  and applies the ack policy (advance on commit, redeliver otherwise, never drop); the policy is a
  pure function tested without a broker, and the whole path is proven against real JetStream by
  `link-nats`'s and `pos-cloud`'s `integration` suites. The binary starts the cursor when a `[nats]`
  config section is present and shuts it down with the HTTP server; absent that section it serves
  reconciliation re-pushes only. ADR-0031 is amended to record the consumer and its testing.
- `pos-cloud` (P7): the **webhook delivery engine** (ADR-0032) — how a tenant receives events as
  signed HTTPS `POST`s. A webhook is **a cursor over the event log, not a queue**: each endpoint
  stores only its position, so a dead endpoint falls behind without the cloud buffering anything (the
  P7 exit criterion), and a failed delivery simply does not advance the cursor. Four safety rails,
  each a separately-tested module: **HMAC-SHA256 signing** over a `"{timestamp}.{body}"` payload with
  a `v1=` header and a ±5-minute replay window (bound into the signature, so a capture cannot be
  re-stamped); **SSRF vetting** that requires https, forbids URL credentials, and refuses any host
  that resolves to a non-public-unicast address (loopback, RFC-1918, the `169.254.169.254` metadata
  range, CGNAT, ULA, IPv4-mapped v6, documentation, reserved, multicast), connecting only to the
  vetted addresses so DNS rebinding cannot slip through; a **circuit breaker** that backs a failing
  endpoint off and **auto-disables** it after 24 hours of continuous failure; and full **per-endpoint
  isolation** (one cursor, one breaker each). The whole engine — signing, replay window, SSRF
  classifier, breaker, and the falls-behind cursor — is unit-tested with no broker, network, or
  database, against explicit crypto and IP-range vectors. The concrete TLS sender (behind a
  `WebhookTransport` seam) and endpoint persistence are deliberately later slices (ADR-0032). Adds
  `url`, `hmac`, `sha2` — all already in the workspace tree, so no new dependency version enters.
- `pos-cloud` (P7): the **four-level configuration tree** (ADR-0033) the cloud owns and publishes,
  resolving `docs/roadmap.md`'s D10 open questions. A store's effective configuration is the deep
  merge of four authored layers — Tenant → Brand → Store → Device, most-specific winning, nested
  objects merged and scalars/arrays replaced. **Deltas are RFC 7386 JSON Merge Patch**: a present key
  overrides, a nested object recurses, and `null` deletes — so a delta can remove a key, and `diff`
  then `apply` round-trips (property-tested over many pairs). A candidate version is **validated in
  the cloud before it is published**, reusing `pos-core`'s §10 inter-flag capability rules
  (`capability::conflicts`) so the cloud cannot bless a flag combination the edge would reject; a
  rejected publish changes nothing, so the **last good version stays current**. A store reports the
  version it holds and gets a **delta when it is within K (default 20) versions of current**, or a
  **full snapshot** when it is further behind or holding a version the cloud no longer retains — the
  "more than K behind ⇒ snapshot" rule made concrete. The engine produces the `ConfigUpdate` values
  the `ConfigStore` port carries and is pure (no persistence, no I/O); its persistence and the admin
  routes are a later slice. `pos-cloud` now composes `pos-core` (pure) for the capability rules.
- `pos-cloud` (P7): **super-admin authentication** (ADR-0034) — the two-factor sign-in guarding the
  admin surface. The password is hashed with **Argon2id** (the same primitive and crate the edge uses
  for PIN hashes; only the PHC hash is stored, never the password), and a **mandatory RFC 6238 TOTP**
  second factor is required — there is no password-only path. TOTP runs over **HMAC-SHA256** (RFC 6238
  permits it, authenticators honour `algorithm=SHA256`), chosen so the cloud reuses the `sha2`/`hmac`
  already in its tree instead of adding a second SHA1 crate version; codes are 6-digit on a 30-second
  step, accepted within a ±1-step skew window, and **single-use** (verification returns the matched
  step and refuses any step at or below the last one used, blocking replay). Both factors are
  evaluated before any verdict and the specific failure is server-log-only, so a prober learns
  nothing about which factor was wrong. The session cookie is **host-only** — `__Host-` prefixed,
  `Secure; HttpOnly; SameSite=Strict; Path=/`, and deliberately **no `Domain`** — so an admin session
  never crosses to another tenant's subdomain (the roadmap's named worst-case isolation failure). The
  auth core is pure and unit-tested with no clock or network: against RFC 6238's SHA256 vectors,
  secret redaction from `Debug`, the mandatory-second-factor/no-oracle rules, replay refusal, and the
  cookie attributes. Adds `argon2` (already at the edge); no new crypto crate for TOTP. The login
  route, credential persistence, TOTP enrolment, and per-tenant API keys are later slices.
- `pos-cloud` (P7 / Track A6): the **retention + PII-masking cron** (ADR-0035) — the data-protection
  enforcement PDPD (Decree 13/2023), GDPR, and CCPA require. Personal data (a marketplace order's
  name/phone/address, a corporate invoice's buyer fields) lives in the `SubjectId`-keyed subject
  store, never in the event log; once a record is past its retention period the cron **masks** it —
  personal field values become `[REDACTED]` while the `subject_id` and timestamps survive, so
  invoices still reference a subject and the books still reconcile. Masking (not row-deletion) is
  chosen precisely to keep that reference; it is one-way and idempotent. The retention period is
  **configuration** (per-country default, ADR-0027), never a code guess. The daily sweep is bounded
  (paged, no whole-table load) and idempotent (only unmasked records are read), and a failed run is
  retried rather than crashing the cloud. Scope is deliberate: it enforces the *automatic, time-based*
  policy over customer/buyer data only — never employee data (there is no behaviour monitoring), and
  it is **not** the path for an individual's erasure/access/portability request, which stays escalated
  to the Data Protection contact. The engine is pure behind a `SubjectStore` seam and unit-tested with
  no database or clock (masking scrubs every value yet preserves the id, is idempotent, leaks no
  original value; the sweep masks exactly the records past retention), using only placeholder test
  data. No new dependencies. The subject-store schema and the runner's wiring into `main` are later
  slices (the corporate-invoice buyer fields land with P10).
- `pos-cloud` (P7): **dashboards answered from materialised rollups** (ADR-0036) — the P7 exit
  criterion (a dashboard answers in under 10 ms). `Cloud::daily_rollups` computes the rollup from the
  log every call (O(events)); the new `dashboard` module keeps the rollup **materialised**: a
  projector cursor folds each event exactly once (idempotent, incremental, rebuildable by resetting
  the cursor), and the dashboard read answers from that stored rollup — its signature takes no
  `EventStore`, so it is O(days) and *cannot* scan the log. Both paths fold with one shared
  `fold_event` (`daily_rollups` was refactored onto it, behaviour unchanged), so the materialised
  rollup equals a full re-scan **by construction** — asserted by a test comparing the two, alongside
  idempotency, incremental-across-appends, and reads-only-the-rollup-store. Pure and I/O-free behind a
  `RollupStore` seam; no new dependencies. The `store-postgres` rollup table, the background projector
  task, and pointing `/v1/stores/{id}/rollups/daily` at the materialised read (same response shape,
  so no OpenAPI change) are the remaining wiring.
- `pos-cloud` (P7): **scoped per-tenant API keys** (ADR-0037) — the bearer credential machine
  integrators present to the public `/v1` API, and the isolation boundary for it. A key is
  `pos_<id>_<secret>`; the cloud stores only `SHA-256(secret)` (a fast hash is correct for a
  high-entropy random token — Argon2 is for guessable human passwords, ADR-0034), verifies in
  constant time, and every key is **bound to one tenant** (`Grant::tenant`, checked against the
  resource) and **deny-by-default by scope** (`Grant::authorizes`), so a key reaches one tenant's data
  and only the capabilities it was granted. Keys are revocable, optionally expiring, and the full
  token is shown once (only the hash is kept); the rejection reason is server-log-only, so it is not
  an enumeration oracle. The `pos_` prefix is fixed so a leaked key trips secret scanners. Lives
  beside the super-admin auth in `auth::apikey`; pure and unit-tested (issue→present→verify
  round-trip, wrong secret, id mismatch, revoked, inclusive expiry, malformed tokens, deny-by-default
  scoping, and no secret leaking through `Debug`), with obviously-fake test secrets. Reuses `sha2`; no
  new dependencies. Key persistence, the provisioning route, and the `/v1` bearer extractor are the
  remaining wiring.
- `pos-cloud` (P7): the **`/v1` bearer authentication seam** (ADR-0037) — `auth::bearer`, the HTTP
  edge over the pure API-key engine. `authenticate` reads the `Authorization: Bearer pos_…` header,
  looks the key up by its public id through a new `ApiKeyStore` lookup seam, and verifies it against
  the clock; `require_scope` then gates the specific action. Two rules are enforced structurally: a
  **no-oracle refusal** — a missing/malformed header, an unknown id, a wrong secret, a revoked or
  expired key all render one indistinguishable `401` (the reason is server-log-only), so a prober
  cannot enumerate keys — and a **store outage answers retryably** (`503`), never as a false denial
  that would make a caller discard a good key. A missing scope is a separate `403`, safe to
  distinguish because identity is already proven. Unit-tested with an in-memory key store and a fake
  clock (valid key → grant, unknown id and bad secret indistinguishable, missing/wrong-scheme header,
  store-outage `503`, ungranted-scope `403`, and all three credential problems rendering the identical
  `401`).
- `pos-cloud` (P7): the materialised-rollup read seam is now **keyed by `(tenant, store)`**
  (ADR-0036). `RollupStore::load`/`save`, `dashboard`, and `project` all take a `TenantId`; because a
  `/v1` caller's tenant comes from its authenticated `Grant` and never from the request, a caller can
  only read rollups for a store within its own tenant, and guessing another tenant's `store_id` reads
  back empty rather than that tenant's data — tenant isolation is a fact of the key, not of a check a
  handler might forget. Library-only refinement (no route or binary yet on it); the from-log
  `Cloud::daily_rollups` reconciliation path is unchanged.
- `store-postgres` (P7): the **deferred cloud persistence** now has its tables and query types.
  Migration `0002` adds `rollups` — one `jsonb` row per `(tenant_id, store_id)` holding the whole
  materialised `StoredRollups` (cursor + per-day counts), so a dashboard read is one primary-key
  lookup, not a log scan (ADR-0036) — and `api_keys` — `id` (the public ULID), `tenant_id`,
  `secret_hash` (bytea, SHA-256), `scopes` (text array of wire names), `revoked`, `expires_at`
  (epoch milliseconds), looked up by primary key (ADR-0037). Both are additive and idempotent; the
  rollup table carries RLS on `tenant_id` like the event log. New `PostgresRollups` and
  `PostgresApiKeys` handles (built from `PostgresStore::rollups()` / `.api_keys()`, sharing the pool)
  hold the SQL and return plain rows; `pos-cloud` implements its `RollupStore` and `ApiKeyStore`
  seams over them in a `persistence` module that does the domain conversion — all SQL in the adapter,
  all conversion in the cloud, no cloud type crossing into the adapter. `Scope` gained wire-name
  mapping (`read_rollups`/`read_events`/`manage_webhooks`, unknown names dropped deny-by-default) and
  `StoredApiKey::from_parts` rehydrates a key from a row.
- `store-postgres` (P7): a **cross-tenant isolation test for the rollup table** on real PostgreSQL —
  tenant A's `save_state` is invisible to tenant B naming the same `store_id` (the `(tenant, store)`
  key is the boundary the `/v1` dashboard rests on), and an `app_tenant`-role session scoped to one
  tenant sees only that tenant's rollup rows via RLS. Alongside the existing event-log RLS cases,
  this closes the "cross-tenant isolation proven by tests" half of the P7 exit criterion. Gated
  behind the `integration` feature; runs in the merge-to-`main` job against `postgres:16`.
- `pos-cloud` (P7): the persistence and the auth are now **wired into the running binary**, so `/v1`
  is a real authenticated, tenant-isolated, materialised-read surface. The router carries one
  `CloudApp` state bundling the event store, the rollup read model, the API-key store, and a
  `SystemClock`; `GET /v1/stores/{store_id}/rollups/daily` now **requires a bearer API key** with the
  `read_rollups` scope (missing/invalid → one indistinguishable `401`, wrong scope → `403`), and
  answers from the **materialised rollup** for the **key's own tenant** — the tenant comes from the
  verified grant, never the request, so a caller reading another tenant's `store_id` gets an empty
  list, never that tenant's data. `main` opens one Postgres pool and takes three views of it (event
  store, rollups, API keys). The internal `/internal/ingest` and `/health` stay unauthenticated
  (private network only). The generated OpenAPI now declares the `api_key` bearer scheme and the
  route's `401`/`403` responses and security requirement (snapshot regenerated). Router tests cover
  the authorised read, a missing key (`401` + `WWW-Authenticate`), a wrong-scope key (`403`), a
  foreign-tenant key reading empty, and a malformed store id (`400`), all against the fakes.
- `pos-cloud` (P7): the **rollup projector** background task, now the single writer of the
  materialised rollup the `/v1` dashboard reads (ADR-0036). Ingest only appends to the event log;
  each interval (configurable, default 30 s) the projector lists the fleet — a new `StoreCatalog`
  seam, answered in `store-postgres` from the distinct `(tenant_id, store_id)` of the log — and
  folds every store's new events into its rollup via the existing `project`, advancing the per-store
  cursor so each event is folded exactly once. Robust: one store's projection failing is logged and
  counted, not fatal; only a failure to list the fleet ends a tick, and the next retries. Wired into
  `main` alongside the ingest cursor and shut down with it on SIGINT. Without it the wired `/v1` read
  would return empty in production, so this is what makes the dashboard slice live. Unit-tested
  against the fakes (a pass folds the fleet then is idempotent; an empty fleet does nothing).
- `pos-cloud` (P7): the **super-admin login is wired** (ADR-0034), turning the pure two-factor check
  into a working `/admin` surface. A new `auth::admin` seam (`AdminStore`) loads the single credential
  and its last-used TOTP step and backs a server-side session table; `POST /admin/login` runs the
  password + mandatory-TOTP check and, on success, mints a **256-bit CSPRNG** session token (via the
  `getrandom` the edge already uses), stores only its `SHA-256`, and sets the host-only `__Host-`
  session cookie. Every credential problem — wrong password, wrong or replayed code, unprovisioned
  admin — collapses to one generic `401` (no oracle), a store outage is a retryable `503`, and the
  matched TOTP step is burned before the session is written so a code cannot mint two sessions.
  `POST /admin/logout` revokes the session and clears the cookie (idempotent); `GET /admin/session` is
  the guard the rest of `/admin` will stand behind. Persistence is `store-postgres` migration `0003`
  (a single-row `super_admin` table and an `admin_sessions` table, neither tenant-scoped — the
  super-admin is global — so neither carries RLS or an `app_tenant` grant); the session TTL is
  configuration (`admin_session_ttl_secs`, default eight hours). `CloudApp` now carries the admin
  store as a fifth collaborator. Adds no crypto crate (`getrandom` was already in the tree). Unit
  tests cover the no-oracle rule, replay refusal, expiry and logout against the fakes; router tests
  cover login→cookie→guard, a cookieless `401`, a wrong password, and logout; and a `store-postgres`
  integration test proves the credential round-trip, the monotonic step advance, and the session
  lifecycle against a real database.
- `pos-cloud` (P7): the **API-key provisioning surface** is wired (ADR-0037), completing the machine
  side of `/v1` auth. Behind the super-admin session guard: `POST /admin/api-keys` mints a CSPRNG id
  (a ULID) and a 256-bit secret at the edge, `issue`s the key, persists only the secret's hash
  (`store-postgres`, migration `0002` table), and returns the full `pos_<id>_<secret>` token **once**
  in the `201` body — it is never recoverable after; `GET /admin/api-keys?tenant_id=…` lists a
  tenant's keys as metadata only (id, scopes, revoked, expiry — never a secret or its hash); and
  `DELETE /admin/api-keys/{id}` revokes idempotently. An unknown scope name on provisioning is a
  `400`, not the silent drop the deny-by-default *read* path applies, so a typo cannot issue a key
  that grants nothing. A new `ApiKeyAdminStore` seam (insert / list / revoke) sits beside the
  read-only `ApiKeyStore`, so the per-request bearer path stays minimal. Adds no dependency. Router
  tests prove a provisioned token then authenticates a real `/v1` read and stops the moment it is
  revoked, that provisioning is closed without a session (`401`), and that an unknown scope is
  refused (`400`); the `store-postgres` integration suite covers insert → list → revoke.
- `pos-cloud` / `store-postgres` (P7): the **config tree now persists** (ADR-0033). A new
  `ConfigTreeStore` seam and the `store-postgres` `config_trees` table (migration `0004`) round-trip a
  store's whole tree — its four authored layers and its published version history — as one `jsonb`
  document per `(tenant, store)`, keyed and RLS-isolated by tenant exactly as the rollup read model.
  The pure engine gains `ConfigTree::state` / `ConfigTree::from_state` to export and rehydrate that
  state: the layers and history come back exactly as stored (so the current version and effective
  document are unchanged across a restart, and the last good version stays current), the validator is
  supplied fresh on load (behaviour, not state), and the history is trusted as already-validated
  rather than re-published. Adds no dependency. Unit-tested on the engine (serialise → rebuild →
  same effective document and same delta/snapshot decision) and against a real database in the
  adapter's integration suite (save → load → upsert → tenant-scoped miss). The admin authoring
  routes and the publish path to a store remain the next slice.
- `pos-cloud` (P7): the **config-tree admin routes** are wired (ADR-0033), behind the super-admin
  session guard. `PUT /admin/stores/{store_id}/config/{level}` (level ∈ tenant/brand/store/device)
  loads the store's tree for the query's tenant, replaces that level's document with the request
  body, and publishes — composing the four layers, validating (including pos-core's §10 inter-flag
  capability rules), and, only if valid, appending a version (a ULID minted at the edge) that is then
  persisted; the `200` carries the new `config_version_id`. An incoherent version — e.g.
  `pay_first_enabled` with `tables_enabled` — is a `422` carrying the violations, and nothing is
  stored, so the last good version stays current. `GET /admin/stores/{store_id}/config` returns the
  current effective (deep-merged, most-specific-wins) document, or `404` if the store has none yet.
  The tenant is named on the query string (the super-admin is global). `CloudApp` gains the
  config-tree store as a sixth collaborator. Router tests cover publish → override → effective-merge,
  the incoherent-config `422`, the cookieless `401`, and the unpublished-store `404`. The publish
  path that delivers a `ConfigUpdate` to a store over the wire remains the next slice.
- `pos-cloud` / `store-postgres` (P7 / Track A6): the **retention / PII-masking cron is wired**
  (ADR-0035). `store-postgres` migration `0005` adds the `subjects` table — the one place personal
  data lives, keyed by a globally-unique `subject_id`, with `collected_at`/`masked_at` as epoch-ms and
  `fields` as jsonb, RLS-isolated by tenant and carrying a partial index for the sweep's "unmasked,
  past cutoff" query — and `PostgresSubjects` implements the `SubjectStore` seam. Masking overwrites
  the field values in the row (the PII is gone from the database, not flagged), and the
  `masked_at IS NULL` write guard makes it idempotent at the database. `main` starts the daily runner
  **only when `retention_days` is configured**: the period is a legal decision, not a code default, so
  with none set the cron stays off (masking on a guessed schedule would erase early or keep too long,
  both violations); the sweep interval defaults to daily. The cron masks (never deletes) so the books
  stay reconcilable, touches only customer/buyer data (never employee data — there is no
  employee-behaviour monitoring), and is not the path for an individual's erasure/access request —
  those stay escalated to the Data Protection contact. Adds no dependency. Proven by a `store-postgres`
  integration test (fetch-due → mask → not-re-fetched → not-re-masked) and a `pos-cloud` runner test
  (one sweep, then clean shutdown). The writer that populates the subject store lands with P10/P11.
- `pos-cloud` / `store-postgres` (P7): **webhook endpoints now persist** (ADR-0032). A webhook is a
  cursor over the event log, so a subscription is only its durable facts — destination, signing
  secret, cursor, disabled flag — never a backlog. `store-postgres` migration `0006` adds the
  `webhook_endpoints` table (`id` ULID PK, `tenant_id`, `store_id`, `url`, `secret`, `cursor` NULL
  until first delivery, `disabled`), RLS-isolated by tenant, with a `tenant_id` index for the admin
  listing and a partial index (`WHERE disabled = false`) for the delivery task's enabled-load. Unlike
  an API-key secret (stored as a hash), the signing secret is kept **in full** because the cloud
  *signs* every delivery with it, so it must be recoverable; `SigningSecret` gains an `expose_secret`
  accessor for the persistence layer alone. `pos-cloud` fills its new `webhook::store::WebhookEndpointStore`
  seam over `PostgresWebhooks`: the tenant-scoped listing never carries the secret, while the delivery
  task loads the enabled fleet **fleet-wide as the trusted role** (RLS bypassed), the same posture as
  the rollup projector and the retention sweep. Adds no dependency. Proven by a `store-postgres`
  integration test (register → tenant-scoped list → fleet-wide enabled load → advance cursor →
  auto-disable suppresses → scoped delete). The admin CRUD routes and the concrete TLS sender remain
  their own later slices (ADR-0032).
- `pos-cloud` (P7): the **webhook admin routes** are wired (ADR-0032), behind the super-admin session
  guard. `POST /admin/webhooks` SSRF-vets the destination first — `https` only, no credentials, and
  every resolved address must be public unicast — running `vet` with a real `getaddrinfo` resolver on
  the blocking pool, then mints a CSPRNG id and signing secret, persists the endpoint, and returns the
  signing secret **once** (the tenant's copy of what the cloud signs deliveries with). A loopback,
  link-local (the `169.254.169.254` metadata range), private, or plaintext URL is a `400` before
  anything is stored. `GET /admin/webhooks?tenant_id=…` lists a tenant's endpoints as metadata only
  (never the secret) and `DELETE /admin/webhooks/{id}?tenant_id=…` removes one within its tenant,
  returning `204` either way — deletion is idempotent and the tenant scope stops one tenant deleting
  another's. `CloudApp` gains the webhook-endpoint store as a seventh collaborator. Router tests cover
  register → list → delete and the SSRF/plaintext refusals over the fakes, using IP-literal
  destinations so vetting needs no DNS. The concrete TLS sender and the dispatch background task remain
  the next slice.
- `pos-cloud` (P7): the **concrete webhook TLS sender** (ADR-0038, a new ADR) — `TlsWebhookSender`, the
  `WebhookTransport` that turns a signed body into one HTTPS `POST`. It is built on the rustls/hyper
  stack **already in the tree** (`hyper`/`hyper-util` via axum; `tokio-rustls`/`webpki-roots`/`ring`
  via async-nats), so it adds direct-dependency lines — `hyper`, `hyper-util`, `http-body-util`,
  `bytes`, `tokio-rustls`, `webpki-roots` — not a new subtree, and no new `cargo-deny` entry. The
  sender **owns its dial**: it opens a TCP connection to one of the endpoint's *pre-vetted* addresses
  and performs the TLS handshake against the URL's hostname, never re-resolving — so it closes the
  DNS-rebinding gap between the SSRF check and the connect by construction. The `ring` crypto provider
  is selected explicitly (and `tokio-rustls` pinned with `default-features = false`) so `aws-lc-rs`
  cannot enter through feature unification; roots are the bundled Mozilla set (hermetic, no
  base-image `ca-certificates` dependency). Each delivery is bounded by a timeout — a black-hole
  endpoint cannot wedge the dispatch loop; a timeout is an ordinary failed delivery. The pure
  request-derivation (host/SNI, origin-form target, `ip:port` dial set, the two signature headers) is
  unit-tested without a network; the handshake belongs to the gated integration lane and the soak. The
  dispatch background task that drives this across the enabled fleet remains the next slice.
- `pos-cloud` (P7): the **webhook dispatch background task** is wired into `main` (ADR-0032), closing
  the webhook feature end to end. Each tick it loads the enabled endpoints fleet-wide (as the trusted
  role, like the projector and the retention sweep), **re-vets** each URL so it can only connect to a
  currently-approved address (closing the DNS-rebinding gap before every delivery batch, not just at
  registration), and delivers the events after each cursor over TLS with the `TlsWebhookSender`,
  persisting each cursor advance so a restart resumes where it left off and persisting a 24-hour
  auto-disable so a dead endpoint drops out of the fleet. The live endpoints — with their cursors and
  **breakers** — are held in memory across ticks (breaker windows accumulate rather than resetting);
  the database holds only the durable facts. It always runs (a cheap no-op with no endpoints) and is
  bounded per endpoint per tick so one far-behind subscriber cannot starve the fleet. Two config knobs,
  both with defaults: `webhook_dispatch_interval_secs` (10s) and `webhook_delivery_timeout_secs` (10s,
  so a black-hole endpoint cannot wedge the loop). The sweep logic is unit-tested over the fakes
  (deliver → persist cursor → idle; a now-unsafe URL is skipped, not delivered to).
- `pos-cloud` (P7): the **config publish-to-store path** (ADR-0039, a new ADR) — the cloud now
  delivers a store its configuration, the half ADR-0033 had deferred. Because the store→cloud link is
  outbound-only (ADR-0031, no cloud→store push channel exists), delivery is a store-initiated **pull**
  on a new store-facing surface: `GET /sync/stores/{store_id}/config?held_version=…` runs the config
  engine's `update_for` and returns `{"status":"up_to_date"}` or `{"status":"update","update":{…}}`
  carrying the RFC-7386 delta (or a full snapshot past K versions behind). It is authenticated by an
  API key with a new deny-by-default `read_config` scope and answers **only for the key's own tenant**
  — the tenant comes from the verified grant, never the path — so a store reaches only its own trees;
  an unknown or unpublished store reads `404`. `/sync` is a fifth route family (store operation, not
  the public integrator API), so it is absent from the OpenAPI document, like `/admin` and
  `/internal`. Reuses the existing API-key bearer and config-tree collaborators — no new dependency or
  `CloudApp` generic. Router tests cover snapshot → up-to-date and the `401`/`403`/`404` closes. The
  `pos_edge` loop that polls this and applies through `ConfigStore` is store-side fleet wiring (P9).
- `pos-cloud` (P7): **reset-cursor-and-replay** for the materialised rollup. `POST
  /admin/stores/{store_id}/rollups/reset?tenant_id=…` (behind the super-admin session guard) saves the
  store's rollup back to the empty default, clearing its per-store projector cursor; the next
  projector pass then re-folds every event from the start of the durable log, so the cloud's read
  model can be rebuilt without touching the event log (`docs/roadmap.md` P7, ADR-0036). `204`
  regardless — a store with no rollup yet resets to the same empty state. Reuses the existing
  `RollupStore` collaborator; a router test proves a seeded cursor is cleared and the cookieless call
  is `401`.
- `pos-cloud` / `store-postgres` (P7): **nightly reconciliation** (ADR-0040, a new ADR) — the cloud's
  emit-missing-ids side. Because ULIDs are not a dense sequence, the cloud cannot know what it dropped
  on its own, so reconciliation is an edge-initiated diff: `POST /internal/reconcile` takes
  `{tenant_id, store_id, event_ids:[…]}` and returns `{missing:[…]}` — exactly the ids the event log
  lacks, which the edge then re-pushes through the idempotent `/internal/ingest`. It lives on the
  private-network `/internal` surface beside ingest (unauthenticated, absent from the OpenAPI). A new
  `ReconcileStore` seam is answered by a `store-postgres` `event_id = ANY(candidates)` membership query
  scoped by tenant and store, bridged through `persistence.rs`; the endpoint is a small
  independently-stated sub-router merged into the main router in `main`, so reconciliation adds **no
  eighth `CloudApp` generic**. Proven by a router test over a fake (missing = candidates − present;
  a non-ULID id is a `400`) and a gated `store-postgres` integration test of the membership query
  (tenant/store-scoped, empty-set short-circuit). The `pos_edge` job that assembles the nightly
  manifest and re-pushes is store-side fleet wiring (P9).
- `pos-cloud` / `store-postgres` (P7): the **printer/KDS discover → propose → admin-approves** flow
  (ADR-0041, a new ADR). A store discovers a device on its LAN and proposes it; a super-admin approves
  it before it is usable — the human gate that keeps an unauthenticated port-9100 device off the
  fleet. `store-postgres` migration `0007` adds `device_proposals` (id, tenant_id, store_id, kind,
  name, address, status, timestamps), RLS-isolated by tenant with a partial index on the pending
  queue, behind a new `DeviceProposalStore` seam and `persistence.rs` bridge. `POST
  /sync/stores/{id}/devices` proposes (store-facing, API key with a new `manage_devices` scope, stored
  `pending`); `GET /sync/stores/{id}/devices` returns the store's **approved** devices — what the edge
  acts on, never a raw discovery. `GET /admin/devices/proposals?tenant_id=…` is the super-admin pending
  queue and `POST …/{id}/approve` / `…/{id}/reject` resolve one (idempotent: only a pending row
  transitions; tenant-scoped so one tenant cannot resolve another's). The routes are a merged
  sub-router carrying their own state, so device onboarding adds **no eighth `CloudApp` generic**
  (same shape as reconciliation). Proven over the fakes end to end (propose → pending queue →
  approve → approved list; scope/`session` closes) and by a gated `store-postgres` integration test
  (propose → list by status → one-way resolve → cross-tenant no-op). The `pos_edge` mDNS discovery
  loop is store-side wiring (P5/P9).
- `pos-cloud` (P7): the **menu-image pipeline** (ADR-0042, a new ADR). `images::render` turns a
  tenant-uploaded image into two JPEG renditions under hard byte budgets — a **≤30 KB thumbnail** and
  a **≤150 KB detail**. Because JPEG size is not a closed form of dimension and quality, each rendition
  walks a descending `(max_edge, quality)` ladder and takes the first attempt at or under budget; the
  ladders end aggressively enough that any real image fits, and an image that somehow does not is a
  `Budget` error rather than an over-budget object. It buys the `image` crate for the codec/resize work
  (ADR-0007: the genuinely-hard-and-general thing to buy), with **minimal features** (`png`+`jpeg`
  only) so `cargo-deny` passes with no new entry. The transform is pure `bytes → bytes` — no I/O — and
  unit-tested (a synthetic image yields two in-budget renditions that both decode; the thumbnail fits
  its pixel bound; a non-image upload is a clean `Decode` error). Where renditions are stored (a
  Postgres `bytea` table rather than the deletion-scheduled `blob-garage` port) and the admin upload
  route are the next slice.
- `pos-cloud` / `store-postgres` (P7): the **translation grid** (ADR-0043, a new ADR) — the cloud-side
  place a tenant authors its localized menu/content strings, feeding the edge's ICU i18n runtime
  (ADR-0020). The grid is `key → { locale → string }`, one `jsonb` per tenant (`store-postgres`
  migration `0008`, RLS-isolated), behind a new `TranslationStore` seam and `persistence.rs` bridge.
  The one structural rule is the always-present fallback: `PUT /admin/translations?tenant_id=…`
  (super-admin session) is rejected `422` naming every key that lacks a non-empty `en`, and nothing is
  stored — so the edge can always degrade to English rather than to a raw key; `GET
  /admin/translations?tenant_id=…` returns the grid (empty if none yet). Authoring is a validated
  whole-grid replace, in a merged sub-router carrying its own state (no `CloudApp` generic). Proven by
  a pure `keys_missing_fallback` unit test and a router round-trip (`PUT` → `GET`, the `422` on a grid
  missing `en`, and the `401` without a session). The store-facing `/sync` fetch that hands the edge
  its grid is the next slice, the same split config delivery drew.
- `docs/roadmap.md` (P7): records the cloud phase as **substantially complete** — every P7 cloud
  feature has landed (adapters; super-admin auth; API keys; the config tree with delta/snapshot
  publishing and store-facing pull; idempotent ingest → rollups + projector; the generated `/v1`
  OpenAPI; the full webhook stack; the NATS cursor feed; reconciliation; reset-cursor-and-replay; the
  image pipeline; the translation grid; the retention cron; device onboarding) and every P7 exit
  criterion is met by a test. Records the deliberate deferrals with their real homes: the store-side
  halves of the cloud↔edge loops (edge pollers) are P9 fleet wiring, and the `metrics-vm`
  sparse-sampling profile waits for metrics integration at scale (the cloud emits no metrics yet, so a
  second sink would have no producer; its stride is sized against P12's measured capacity).
- `deploy/` (P8, ADR-0044 — a new ADR): the **fork-and-deploy stack** for one country cell on a single
  VPS. `deploy/Dockerfile` builds `pos_cloud` multi-stage (a Rust 1.94.1 builder → a `debian:bookworm-slim`
  runtime carrying only the stripped binary and CA roots, running as a non-root uid `10001`).
  `deploy/compose.yml` runs the four P7 backends (`pos_cloud`, `postgres:16.4`, `nats:2.10` with
  JetStream, `dxflrs/garage`) behind a Caddy TLS ingress — every image pinned to an immutable tag, each
  service with log-size/file and ~memory/CPU/pids caps (~1.4 GB across the four), on an `internal`
  backend network so only Caddy faces the internet. `deploy/Caddyfile` + `deploy/caddy.Dockerfile`
  (Caddy xcaddy-built with the Cloudflare DNS provider) terminate real TLS: DNS-01 by default (grey-cloud
  the record, no inbound `:80` to ACME), with a documented `<vps-ip>.sslip.io` HTTP-01 fallback for a box
  with no purchased domain; Cloudflare "Flexible" SSL is forbidden. Operational secrets are **not** in the
  repo — `deploy/compose.yml` reads `deploy/secrets/{pos.env,cloud.toml,garage.toml,caddy.env}`, all
  git-ignored and generated on the box by `bootstrap.sh` (P8b, next). A root `.dockerignore` keeps the
  build context to the source cargo needs.
- `deploy/bootstrap.sh` (P8, ADR-0044): the idempotent server-side bootstrap. It mints the internal
  operational secrets **on the VPS** — the PostgreSQL password (`pos.env`), the `pos_cloud` config
  (`cloud.toml`, chowned to the app's uid `10001` so a `600` file stays readable by the non-root
  container), a NATS JetStream token (`nats.conf`, now enforced on the internal network), and a Garage
  `rpc_secret` (`garage.toml`) — writes them `600` under the git-ignored `deploy/secrets/`, and brings the
  stack up. Re-running keeps every existing secret rather than rotating it, so a second run cannot lock
  the box out of a database whose password was already deployed. Only the TLS values (`DOMAIN`,
  `ACME_EMAIL`, `CF_DNS_API_TOKEN`) come from outside, passed in the environment on the first run and
  written to `caddy.env`; an `*.sslip.io` `DOMAIN` needs no Cloudflare token (HTTP-01 fallback). It prints
  a **one-time super-admin setup token** once — the token that enrols the first super-admin through the
  first-boot provisioning route wired in P8c; `AdminStore` has no credential-writer yet, so that route,
  and the `reset_admin` break-glass beside it, are P8c's slice. No application code changed here.
- `pos-cloud` / `store-postgres` (P8, ADR-0045 — a new ADR): **first-boot super-admin enrolment**, the
  route that consumes the setup token above. `POST /admin/setup` provisions the single super-admin the
  first time — token-gated and self-disabling — closing the gap ADR-0034 left ("first-boot seeding, P8
  bootstrap"): the login existed but nothing could write the first credential. It compares the
  configured `admin_setup_token` in constant time, then hashes the operator's chosen password with
  Argon2id under a fresh CSPRNG salt, generates a 256-bit TOTP secret, and returns the enrolment (the
  `otpauth://` URI and its base32 secret) exactly once. It answers `404` when no token is configured,
  `401` on a token mismatch, `422` below a 12-character password, and `409` once an admin exists —
  provisioning is `INSERT … ON CONFLICT DO NOTHING` on the single-row `super_admin` table, so the first
  enrolment wins and the token is thereafter inert. `AdminStore` gains a `provision_credential` writer
  (backed by `store-postgres`; **no new migration** — the table already had the shape from migration
  `0003`), and a new `crate::auth::enrol` hand-rolls RFC 4648 base32 and the `otpauth://` URI (no new
  dependency). Proven over the fakes: enrol → `201` with a well-formed enrolment and a credential now
  present, a second call `409`, and the `404` / `401` / `422` refusals each provisioning nothing. The
  reset break-glass is a DB one-shot in the deploy workflow (next), gated by a GitHub Environment, so
  no `reset_admin` flag ever rides in the app's own container environment.
- `deploy/` (P8, ADR-0044/ADR-0045): the **deploy workflow** and the reset break-glass.
  `.github/workflows/deploy.yml` (manual `workflow_dispatch`, in a `production` GitHub Environment)
  builds the `pos_cloud` and Caddy images in CI, ships both over the existing SSH channel
  (`docker save | ssh docker load` — no registry, so GitHub still holds no application secret), and
  runs `bootstrap.sh` on the box with the loaded image tags and `POS_BOOTSTRAP_NO_BUILD=1` so the box
  never rebuilds from source. A rollback is re-running at an older commit; the app container is
  stateless. `bootstrap.sh` now writes the setup token as `admin_setup_token` into `cloud.toml` (where
  `pos_cloud` reads it) and gained the `POS_BOOTSTRAP_NO_BUILD` path. `reset_admin=true` runs
  `deploy/reset-admin.sh` after bring-up — `DELETE FROM super_admin; DELETE FROM admin_sessions;`, the
  ADR-0045 break-glass — gated by the Environment's required reviewer, the second human a wipe needs.
  The fork's 4–6 secrets (`VPS_HOST` / `VPS_USER` / `VPS_SSH_KEY` / `VPS_KNOWN_HOSTS` / `DOMAIN` /
  `ACME_EMAIL` / `CF_DNS_API_TOKEN`) are documented in the workflow and `deploy/README.md`; the SSH
  host key is pinned (`VPS_KNOWN_HOSTS`), not trust-on-first-use.
- `deploy/` (P8, ADR-0046 — a new ADR): **cloud backups and the restore drill**. Postgres now archives
  WAL continuously (`archive_mode=on` to a `wal_archive` volume, a fail-closed `archive_command`);
  `deploy/backup.sh` streams a compressed `pg_dump` out of the container and ships it off-box with
  `rclone` when `RCLONE_REMOTE` is set; the deploy workflow takes a `--label pre-update` snapshot before
  each new image comes up. Four unequal backup classes — continuous WAL, the daily dump, a weekly Garage
  object sync, and the pre-update snapshot — price each by its real recovery value. `deploy/restore-drill.sh`
  is the proof a backup restores: it dumps the live database, restores it into a throwaway one, and
  reconciles every public table's row count against the source, exiting non-zero on any mismatch — a
  silently unrestorable backup fails loudly. `nightly.yml`'s `restore-drill` job now runs it for real
  against a service Postgres seeded with a synthetic dataset (it was a placeholder echo). The
  store-backup half of the drill is edge WAL shipping (P9, spike A4) and joins when that lands.
  `/backups/` is git-ignored — a dump holds T1/T2 data and must never be committed.
- `k8s/` + `docs/deploy-runbook.md` (P8, ADR-0044): the **optional Kubernetes lane** and the
  **fork-to-UI runbook**. `k8s/pos-cloud.yaml` mirrors the Compose stack as a starting skeleton — the
  four backends as Deployments with PVCs and the Recreate strategy, `pos_cloud` running as the
  non-root uid `10001` with a `/health` readiness probe, an Ingress for DNS-01 TLS — with every
  environment-specific line (`storageClassName`, `ingressClassName`, the issuer, the image) marked
  `# ADAPT`; `k8s/README.md` documents the create-secrets-in-cluster model and that Compose stays the
  supported default. `docs/deploy-runbook.md` is the ordered checklist a forker follows: the
  Cloudflare rules (grey-clouded record, a scoped DNS token, "Flexible" SSL forbidden), the 4–6 GitHub
  secrets, the `production` Environment with a required reviewer, running the deploy, and enrolling the
  first super-admin. `docs/roadmap.md` records P8 as **substantially done**, with the store-backup half
  of the restore drill (P9) and the human end-to-end fork test as the noted deferrals.
- `crates/adapters/updater-minisign` (P9, ADR-0047 — a new ADR): the **minisign update-verification
  adapter**, the concrete `Signer` the P2 port anticipated ("real verification is minisign over a vetted
  Ed25519 crate; it will pass the same suite"). `MinisignVerifier` verifies a release artifact's
  signature over `ed25519-dalek` and `blake2` (both pure-Rust, no C), parsing minisign's binary blob —
  `algorithm(2) ∥ key_id(8) ∥ ed25519_sig(64)` — and both the `Ed` (raw) and `ED` (BLAKE2b-512 prehash)
  algorithms. It is **verify-only**: there is no `sign` method and it never holds a private key
  (`docs/architecture.md` §4 keeps signing offline). It honours the port's three status distinctions
  exactly — a wrong key id is `invalid_argument` ("try the other baked-in key"), a bad signature for the
  right key is `permission_denied` (terminal, never auto-retried into an install), and malformed bytes
  are `invalid_argument` — and is **total over hostile input**: every parse is a checked `slice.get`,
  under the backbone's `panic`/`indexing_slicing`/`unwrap`/`expect` denials, because verification runs at
  startup on attacker-chosen bytes. It passes the existing `signer` contract suite (the harness signs
  with throwaway seed-derived keypairs — real signatures, never a production key), plus a legacy-`Ed`
  round trip. `ed25519-dalek`/`curve25519-dalek`/`blake2` enter the graph and `cargo deny` stays green.
  The production keypairs remain the human-only offline step (P0/P9); whether a valid key is still
  trusted stays a revocation-list question for the config tree, and how an update rolls out is ADR-0048
  (next).
- `pos-core` (P9, ADR-0048 — a new ADR): the **OTA rollout decision**. `pos_core::ota::decide_rollout`
  is the pure function that answers "should this device install this update *now*?", once the artifact
  is validly signed (P9a). It weighs the device's version / ring / canary-bucket / last-self-test
  against the cloud's published update by a fixed precedence that *is* the safety argument — roll back
  (a device that failed its self-test on the running version reverts, outranking even the kill switch),
  already-current, halt (kill switch), refuse (revoked signing key), ring gate, fleet canary gate,
  install. The rollout shape is **published data** — `min_ring` (Lab < Pilot < Fleet) plus a
  `fleet_rollout_percent` that a stable per-device canary bucket is measured against — which settles the
  docs' inconsistent ring count: adding a "25% ring" is setting a number, not shipping a release. The
  logic is pure and stateless (the device persists only its current version and last self-test), so the
  simulator (P12) can exhaust it, and the signing key id is the raw `[u8; 8]` minisign uses, keeping
  `pos-core` off `pos-ports` (the edge maps `KeyId` at the boundary). Proven by twelve tests binding each
  precedence rule and the canary ramp. The `.pre-update` DB copy and the act of installing or reverting
  are edge I/O (P9e); revocation-list delivery is the config tree (ADR-0033).
- `pos-core` (P9, ADR-0049 — a new ADR): the **single-active lease** and its invoice-range handoff.
  `pos_core::lease` decides who is the active machine and hands a replacement a fresh invoice-number
  range, as pure functions. `lease_standing(held, authoritative)` compares two `LeaseGeneration`s and
  nothing else — **no clock** — so a lease never expires while the store is offline: equal ⇒ `Active`,
  behind ⇒ `Superseded` (the old machine goes read-only), ahead ⇒ `Invalid` (a generation the store
  never issued is refused, not trusted). `issue_replacement` starts the replacement's `InvoiceRange`
  exactly where the previous one ended, so the two are **disjoint by construction** — even a window
  where a still-offline old machine keeps issuing invoices cannot mint a legal invoice number the new
  machine also mints. Pure `pos-core`, no `pos-ports`; the wire `LeaseToken` stays the credential and
  the generation is the order that decides supersession. The actual invoice-range allocation is a
  `Fiscalization` call at the cloud (P10/A2), honouring `Superseded` → read-only is the edge (P9e), and
  legal invoice numbering stays distinct from the per-store gapless receipt counter (ADR-0025). Proven
  by tests binding the generation verdict, its time-independence, and the disjoint-forward range
  invariant across a chain of swaps.
- `pos-core` (P9, ADR-0050 — a new ADR): the **activation-code exchange**, the pure half of turning a
  fresh box into a trading machine. `pos_core::activation::ActivationCode` is a short human-typed code —
  eleven Crockford base-32 payload symbols (55 bits) plus a checksum symbol, shown as `XXXX-XXXX-XXXX` —
  that `parse` normalises (case-, hyphen- and whitespace-insensitive, folding the ambiguous `I`/`L`/`O`
  glyphs) and checksums, so a mistyped code fails at the keyboard rather than after a round-trip; the
  checksum is a typo guard, not authentication. `redeem` is the single-use rule: it grants only a code
  the cloud still records as `Issued`, refusing `Redeemed` (a spent sheet) and `Revoked` (a cancelled
  one), and a grant obliges the caller to flip the record in the credential-minting transaction — the
  atomicity is the cloud's, the rule the domain's. `device_activation` states that a box holding its
  `SecretName::DeviceCredential` is activated. Pure `pos-core`, no `pos-ports`; the code's `Debug` is
  redacted since it is a bearer credential until redeemed. Deliberately elsewhere (P9e): the cloud
  issue/look-up/consume endpoint and the edge client that presents the code over `MessageLink`, stores
  the credential via `KeyVault`, and emits `device.activation.completed`.
- `pos-cloud` + `store-postgres` (P9, ADR-0051 — a new ADR): the **cloud activation exchange**, the
  I/O half of ADR-0050. A super-admin issues an activation code bound to a device slot
  (`POST /admin/activation-codes`, shown once) and can cancel a slot's pending code
  (`POST /admin/activation-codes/revoke`); a fresh box presents its code on the **unauthenticated**
  `POST /activate` — the code is the credential — and receives a long-lived `posdev_<id>_<secret>`
  device credential in exchange. Redeem-and-mint is one transactional adapter method
  (`consume_and_provision`): `UPDATE … WHERE status = 'issued' RETURNING <slot>` is the single-use
  guard, and the `device_credentials` row is inserted for that slot in the same transaction, so a code
  can never be spent without minting exactly one credential for its own slot (composed seam calls
  cannot share a transaction, ADR-0016). The credential is api-key-shaped: high-entropy, `SHA-256`
  stored (not Argon2, ADR-0037), shown once; the code is stored only as `SHA-256` of its canonical
  text. Spent, revoked, unknown, and raced codes all collapse to one generic `403` (no oracle); a
  malformed code is a plain `400`. New: `pos_core::activation::ActivationCode::from_entropy` (code
  generation stays at the I/O edge), the `ActivationCodeStore` seam, `store-postgres` migration
  `0009_cloud_activation.sql` (`activation_codes` + `device_credentials`, no RLS — the api-key
  PK-lookup pattern), and the `PostgresActivationCodes` adapter. The edge client and the
  device-credential verification path are P9e; `device.activation.completed` is emitted edge-side, not
  in the exchange transaction. Proven by a router test (single-use + no-oracle) and a `store-postgres`
  round-trip (issue → atomic redeem+mint → replay-refused → revoke).
- `pos-core` + `pos-cloud` (P9, ADR-0052 — a new ADR): the **OTA rollout is published as
  configuration**. Rather than a new push channel, the cloud publishes the rollout through the existing
  config tree (ADR-0033/ADR-0004) as two keys — `fleet_update` (`target_version`, `min_ring`,
  `rollout_percent`, `halted`, `signing_key_id`, `revoked_key_ids`) fleet-wide, and `device_ota`
  (`ring`, `canary_bucket`) per device — and each store pulls it like any other setting. The schema and
  its rules live in `pos-core`: `FleetUpdateConfig`/`DeviceOtaConfig` are typed views whose `validate`
  methods parse the keys into the `PublishedUpdate`, revoked-key list, and `(Ring, bucket)` that
  `decide_rollout` (ADR-0048) consumes, so the cloud validates on publish and the edge validates on
  receipt through the *same* code and cannot disagree. `pos-core::ota` also gains `Ring::from_wire`/
  `as_wire`, `ReleaseVersion::parse`, and `parse_signing_key_id`. The cloud's `CapabilityValidator` now
  rejects an incoherent `fleet_update`/`device_ota` on publish — reporting every violation at once (a
  bad version, ring, ramp percent, or key id) — while an absent key means simply "no rollout
  configured", never an error. No new dependency, no new port. The edge updater that reads these keys
  and drives `decide_rollout` (verify → self-test → rollback) is P9e-4; fetching the update artifact
  bytes rides the edge→cloud transport still to be decided.
- `pos-ports` + `pos-contract-tests` + `pos-fakes` (P9, ADR-0053 — a new ADR): a **seventeenth port,
  `CloudSync`** — the store's request/response channel to the cloud, distinct from the deliberately
  outbound-only `MessageLink` (so the store still sells offline). It carries the two calls that need an
  answer back: `activate(code) -> {device_id, credential}` (the first-boot activation exchange,
  ADR-0050) and `fetch_update(release) -> bytes` (the OTA artifact, ADR-0048, verified against the
  minisign `Signer` before it is trusted — a transport is not a trust boundary). It names no domain
  type — the code arrives as a `&str` and the credential comes back as a `Secret` — so `pos-core` stays
  a sibling; and it is compile-time selected, so it has no `Dyn` mirror. `PortName` gains `CloudSync`
  and the port count moves to seventeen (`docs/architecture.md` §5 is authoritative; ADR-0021 amended,
  its immutable body preserved). A `CloudSync` contract suite and `FakeCloudSync` land with it —
  carried by the `SUITES` table and the every-port / every-suite assertions — so the count is enforced,
  not just documented. **No new dependency**: the port names only `pos-proto` and its own
  `Secret`/`PortError`. The real HTTP adapter that speaks to the cloud, and the edge activation client
  and OTA updater that consume the port, are P9e-2b onward.
- `cloud-sync-http` (P9, ADR-0054 — a new ADR): the **edge→cloud HTTP client**, the concrete
  `CloudSync` adapter the store composes. `activate(code)` posts `{code}` to the cloud's `POST /activate`
  (ADR-0050) and reads `{device_id, credential}` back; `fetch_update(release)` posts `{release}` to
  `POST /internal/ota/artifact` and reads the signed artifact bytes back (verified by the `Signer`
  downstream, not here — a transport is not a trust boundary). The socket lives behind an
  `HttpTransport` seam; everything else — request-shaping and the status→`PortError` mapping (`400` →
  `invalid_argument`, `403` → `permission_denied` with no oracle, `404` → `not_found`, else →
  `unavailable`) — is pure, so the shared `CloudSync` contract suite runs in the pull-request gate
  against a stub transport while the real TLS path (`TlsHttpTransport`) belongs to the gated integration
  lane and the soak. **No new dependency and no new `cargo-deny` entry**: the client reuses the exact
  rustls/hyper stack the webhook sender pins (ADR-0038) at the versions already in the tree. The cloud's
  `/internal/ota/artifact` route is defined by ADR-0054 and served by its P9e-4 counterpart.
- The ADR index (`docs/adr/README.md`) is caught up: ADRs 0038–0054 were missing from the table and are
  now listed.
- `pos-edge` (P9, ADR-0050/ADR-0053): the **edge device-activation flow and boot gate**. A generic
  `activation_router` composes `CloudSync` (exchange the code) and `KeyVault` (store the credential)
  as a sub-router — the same shape the cloud's activation routes take, since both ports are
  compile-time-selected with no `Dyn` mirror and cannot ride the concrete `AppState`. `POST
  /api/activate` parses the code locally (ADR-0050 — locally checkable), refuses a box that already
  holds a credential as a `409` (never a re-exchange of a now-spent code), exchanges it, stores the
  credential under `SecretName::DeviceCredential`, and records `device.activation.completed` via a new
  `Edge::record_activation` (a system event: no employee at first boot, the box's new identity is both
  the reporting and the activated device). `GET /api/activation` and `boot_standing` report the
  standing straight from the vault, which is the source of truth for "activated". Proven against the
  in-memory fakes and a stub cloud (`crates/pos-edge/tests/activation.rs`): a valid code activates and
  broadcasts the completion, the standing flips, a second attempt is a conflict, a wrong code is a
  no-oracle `403`, and a malformed one is rejected locally with nothing stored. Composition into the
  shipped binary waits on the OS-keyring `KeyVault` adapter, deferred as a gated hardware/OS handoff
  (it needs a live credential store its contract suite cannot get in the pull-request gate).
- `pos-edge` (P9, ADR-0055 — a new ADR): the **edge OTA updater**, the on-box orchestration that
  carries an eligible update through. `OtaUpdater` composes `CloudSync` (fetch the artifact), the
  `Signer` port (verify), and a new `UpdateInstaller` seam (the real-machine steps). `run` is a fixed,
  safety-ordered sequence: a `Superseded`/`Invalid` lease (ADR-0049) reports `ReadOnly` and touches
  nothing; `decide_rollout` (ADR-0048) resolves roll-back / halt / refuse / skip / install; and on
  install the artifact is **verified before the disk is touched** — read the claimed key id, refuse a
  revoked one, select the matching baked-in key, check the signature — then `stage_backup` (the
  `.pre-update` copy) → `apply` → `self_test`, committing only on a pass and rolling back on a fail.
  The five `UpdateInstaller` methods are the gated hardware/OS steps (write the binary, reboot, run the
  smoke test), left to the shipped binary and a real box; the orchestration around them is proven
  against the in-memory fakes and a recording installer (`crates/pos-edge/tests/ota.rs`, nine cases),
  which assert both the outcome and that a bad signature or an untrusted key writes nothing, and a
  failed self-test rolls back rather than commits. Composition into the binary waits on that real
  installer and the minisign keypair, both gated (`docs/roadmap.md` P9).
- `pos-cloud` (P11, ADR-0056 — a new ADR): the **public order-intake surface**, `POST /v1/orders` over
  the `OrderIn` port — the shared path `docs/roadmap.md` P11 builds first because the marketplaces and
  QR ordering (ADR-0012) all reuse it. `OrderIn` is a driving port (ADR-0026 §5): the store's edge
  implements it, and this endpoint is a caller (in the binary, the cloud→store relay; in tests,
  `FakeIntake`). A new `Scope::PlaceOrders` gates it, a `StoreDirectory` seam binds the request's store
  to the key's tenant so a key can never place an order into another tenant's store (unknown and
  not-yours both a generic `404`, no oracle), and the full `OrderIn` contract is surfaced unchanged: a
  first accept is `201`, an idempotent repeat `200` with `created:false`, an unknown item `400`, a
  rate-limit `429`, and a stale quote is *reported* (`repriced`) rather than honoured. A dedicated
  `OrderRequest`/`OrderResponse` DTO owns the wire shape (`pos-proto` carries no `utoipa`), and a
  guest note rides in as a `GuestNote` that cannot reach the event log. Proven against `FakeIntake` and
  a fake directory (`crates/pos-cloud/tests/cloud.rs`): accept, idempotent repeat, unknown item,
  cross-tenant and unknown store, missing scope, no bearer, and a QR order that awaits staff
  confirmation with a repriced stale quote. Serving it in the binary and registering it in
  `docs/openapi.json` lands with the cloud→store relay (P11a-2) — a buildable follow-on, not an
  external blocker; P10 (`countries/vn`) stays blocked on A2, and the payment (A1) and Grab/ShopeeFood
  (A3) adapters stay blocked-external.
- `pos-cloud` (P11, ADR-0057 — a new ADR): the **QR ordering** security keystone and guardrail
  decision. A guest scanning a printed table code has no API key, so the `table_id` travels as an
  HMAC-signed token — `{tenant}.{store}.{table}.{hex_tag}`, on the same `hmac`/`sha2` line and
  constant-time-compare idiom the webhook signer uses (ADR-0038), no new dependency —
  `mint_table_token` (admin side, printed into the QR) and `verify_table_token` (returns the
  `TableRef` or refuses a forged/tampered/cross-store token). The four guardrails ADR-0012 requires
  are one pure, total `evaluate(QrFacts) -> QrDecision` in a fixed precedence: a forged token is
  `UntrustedTable` first, then an offline store (`StoreOffline` — "ask a member of staff"), then
  out-of-hours, then the per-table rate limit; otherwise `Accept` carrying the staff-confirmation
  default (on). Both halves are pure and exhaustively unit-tested (`crates/pos-cloud/src/qr.rs`): the
  token mint/verify (known-shape, tamper, wrong secret, cross-table replay, malformed) and every
  guardrail branch and its precedence. The endpoint that gathers the facts (store-online, business
  hours, rate limiter) and relays a verified QR order into the `OrderIn` intake (ADR-0056) lands with
  the P11a cloud→store relay (P11b-2) — a buildable follow-on, not an external blocker.
- `shipping-ahamove` (P11, ADR-0058 — a new ADR): the first **`ShippingDispatch`** adapter, one of the
  two couriers `architecture.md` §6.1 names. It maps the port's three operations — book, cancel, track
  — onto a REST courier API behind a `CourierTransport` seam, exactly the transport-seam split the
  webhook sender (ADR-0038) and the edge→cloud client (ADR-0054) use: the socket lives behind the seam
  (`TlsCourierTransport`, the tree's pinned rustls/hyper stack, one trusted configured host so no SSRF
  surface), and building the request, mapping the courier's status vocabulary to `ShipmentStatus`, and
  mapping HTTP status to `PortError` are all pure. The shared `ShippingDispatch` contract suite runs in
  the fast gate against a **stateful stub courier** with no socket, proving the port semantics that do
  not change when a field is renamed: idempotent booking (`shipment_id` as the courier's idempotency
  key — a timed-out retry books no second rider), a cancel of a completed job refused
  `failed_precondition` rather than a quiet success that promises a refund, an already-cancelled job's
  cancel succeeding, an unknown job `not_found`, and a finished job still trackable so a missed
  callback reconciles. An unmapped courier status stays an unrecognised `Open<ShipmentStatus>` (safe:
  non-terminal), never coerced. The delivery contact (recipient name/phone/address) is VN resident
  personal data transmitted to the courier — a data processor under PDPD, lawful basis contract
  performance — for the sole purpose of delivering, never logged, and never carried back on the tracked
  shipment. The exact Ahamove endpoint strings are pinned in the gated integration lane; `templates/adapter-template`
  is extracted at the third integration adapter (`erp-sap`), not before (the rule of three, roadmap P11).
- `erp-sap` (P11, ADR-0059 — a new ADR): the **`ErpSink`** adapter, the second integration adapter and
  the first that is not a courier — proving the transport-seam pattern generalises past one port shape.
  It maps the port's two operations — post a whole day, read back what posted — onto a REST ERP API
  behind an `ErpTransport` seam (the same shape the couriers use, ADR-0058), with request shaping and
  HTTP→`PortError` mapping pure and the socket behind `TlsErpTransport` (one trusted configured host,
  no SSRF surface). The shared `ErpSink` contract suite runs in the fast gate against a stateful stub
  ERP with no socket, proving the three obligations: idempotency by revision (`store:business_date:revision`,
  a retried nightly job harmless), a higher revision superseding a lower one for the same day (an
  adapter that appended would double-count revenue), and whole-or-nothing validation (an unknown
  account fails the entire batch `invalid_argument` with nothing posted — half a day in a period is
  worse than none). Postings are keyed by the **trading** day (`business_date`), the deliberate
  opposite of `fiscal-vn`'s calendar-date keying. Posting is nightly and off the sales path (ADR-0001),
  and carries no personal data (aggregates by account and day). Two small additive read-only accessors
  land on the port types to let an adapter serialise a line without matching the `#[non_exhaustive]`
  `ErpLine` from outside `pos-ports`: `ErpLine::{kind_wire, amount, quantity}` and `Quantity::as_milli`.
  The exact SAP strings are pinned in the gated integration lane; `erp-sap` is the third integration
  adapter's second divergent prior (with the courier) for the `templates/adapter-template` extraction.
- `shipping-grabexpress` (P11, ADR-0058): the second **`ShippingDispatch`** courier, the other one
  `architecture.md` §6.1 names, built from the new `templates/adapter-template`. Same transport-seam
  shape, same pure status mapping, same stub-driven contract suite as `shipping-ahamove` — differing
  only in Grab Express's own wire (a booking keyed by `merchant_order_id`, a `deliveries` collection,
  its own status vocabulary `ALLOCATING`/`PICKING_UP`/`IN_DELIVERY`/`COMPLETED`/`CANCELED`, mapped onto
  `ShipmentStatus` with unknowns kept unrecognised and non-terminal). It passes the shared
  `ShippingDispatch` suite (all 8 cases) against a stateful stub courier, plus per-status unit tests.
  No new ADR — it is the second instance under ADR-0058.
- `templates/adapter-template/` (P11, roadmap P11 "rule of three"): the extracted scaffold for a new
  network adapter, generalising the pattern now shared by `shipping-ahamove`, `shipping-grabexpress`,
  `erp-sap`, and `cloud-sync-http` — a transport seam (the only thing that touches a socket) plus a
  `Tls…Transport` on the tree's pinned rustls/hyper stack, a pure core (request shaping + status→`PortError`
  mapping), and a stub-driven contract test. It is deliberately **not** a workspace member: the source
  files carry a `.tmpl` suffix and there is no real `Cargo.toml`, so Cargo, `clippy`, `rustfmt`, and
  `cargo test` never see it — a contributor copies it out, drops the suffix, fills the placeholders,
  and adds the crate to `members`. A `README.md` carries the shape, the copy-out checklist, and links
  to the four worked examples.
- `pos-simulator` (P12): the **executable capacity model** (`crates/pos-simulator/src/capacity.rs`), the
  first slice of the virtual-fleet simulator. `docs/capacity-and-reliability.md` §2's three scenarios
  (A/B/C) are encoded as data and its sizing formulas as pure integer functions, so the published
  numbers become a *checked* artifact rather than an estimate on a page: events/day is reproduced
  exactly (recovering the table's own implied 8-events-per-bill constant), PostgreSQL storage within 5%
  (the model gives B 75 GB where the table rounds to 72), daily bandwidth lands inside each published
  range (from QR sessions × ~1 MB of menu imagery — scenario B's ~250 GB/day wall), and the §2
  ÷1260 peak-ingest formula is shown to be a conservative ceiling every scenario sits under. A
  `reconcile()` returns the derived-vs-published divergences instead of hiding them, and pins the one
  place the estimates do not reconcile — scenario A's 9,000 QR sessions/day against the 18,000 the
  bills×QR-share model gives (B and C both agree with the share), filed for the pilot to settle. All
  integer, no floating point on a capacity number; own `[lints]` table like the binaries. The fleet
  behavioral scenarios (OTA rings, offline drain, config fan-out, reconciliation) are the next slice;
  the real sustained soak on the target VPS with live PostgreSQL/NATS is an operations/P13 handoff, not
  faked here.
- `pos-simulator` (P12): the **virtual-fleet behavioral scenarios** (`fleet`, `stress`). `fleet` folds
  the framework's *real* OTA decision — `pos_core::ota::decide_rollout` (ADR-0048) — over a whole fleet
  and returns the aggregate, so the canary ramp, the kill switch reaching every ring, a revoked key
  refused fleet-wide, and a failed self-test rolling one device back are each asserted across the fleet
  rather than one device (the scenarios `crates/pos-core/tests/ota_rollout.rs` seeded now have their
  home). `stress` makes §4's behavioural stress tests executable: the offline drain reproduces the
  "200 stores → 800k events" figure from the fleet model and shows the ~9-minute drain is feasible
  within the ingest ceiling (and conservative); the webhook backpressure model shows a dead endpoint's
  cursor falls behind linearly while its in-memory footprint stays one batch, whatever the outage; and
  the nightly reconciliation is the sorted set-difference (`store − cloud`) of missing ids to re-push.
  17 tests in all; still integer-only and deterministic (rates and counts are inputs, no clock).
- `pos-simulator` (P12): the **runnable entry and reporting** (`report`, `main`, `just simulate`), plus
  the P12 exit recorded honestly. `report` renders the capacity envelope and the reconciliation report
  as text (pure, unit-tested); the `pos-simulator` binary prints them and is the one place `print` is
  allowed. `docs/capacity-and-reliability.md` gains a §8 stating the numbers are now executable and
  self-checked, and pins the one standing discrepancy (scenario A's 9,000 QR sessions/day vs the
  model's 18,000) for the pilot rather than silently changing it. `docs/roadmap.md` records P12's
  status: the deterministic core (capacity model + fleet scenarios) is done, while *"scenario B
  reproduced on target hardware"* and *"the soak runs nightly"* are a pilot/operations handoff (like
  A4/A5), not faked. The same status note records P11's: the unblocked surface is done, and serving the
  intake awaits a cloud→store API-contract decision (the cloud cannot implement `OrderIn`, and the tree
  is store-initiated only).
- `docs/guides/` (P13): four **task-shaped guides**, each finishing in one sitting and each linking the
  worked examples and the CI enforcement it names — [start from zero](docs/guides/start-from-zero.md)
  (run `pos_edge` on a laptop with `just run-edge`, then deploy the cloud tier to one VPS by the
  six-secret sslip.io path), [write an adapter](docs/guides/write-an-adapter.md) (copy
  `templates/adapter-template`, map each provider status to a `PortError`, pass the port's contract
  suite), [add a country module](docs/guides/add-a-country-module.md) (the three ADR-0027 edits that
  `cargo xtask countries` enforces), and [run the simulator](docs/guides/run-the-simulator.md) (`just
  simulate` and the tests behind it). A `docs/guides/README.md` indexes them and states the two-tier
  mental model (store `pos_edge` vs cloud `pos_cloud`). Walking the fork-to-UI checklist against the
  real artifacts is what these guides do, and it surfaced the two `Fixed` items below.
- `docs/roadmap.md` records P13's status: the repository-only half (the four guides, and the fork-to-UI
  walk that produced the fixes below) is done, while *"a pilot store trades a full day"* and the hardware
  matrix exercised for real need physical stores and procured hardware (A5) — a pilot/operations handoff,
  like the WAL-on-Windows soak (A4), that this documentation sets up rather than performs.
- `deploy` workflow: an **optional `VPS_PORT` secret** for a VPS whose SSH is not on 22. It defaults to
  22 when unset or empty (so existing forks are unaffected), and is threaded into every `ssh`/`scp` step
  (`-p`/`-P`). The runbook and Start-from-zero note that `VPS_KNOWN_HOSTS` for a non-default port must be
  generated with `ssh-keyscan -p <port>`, whose entries are keyed `[host]:port` — the form SSH looks up.

### Changed
- **Deploy images are cross-compiled on the runner, no longer built under QEMU emulation.** The
  arm64 support added above built the non-native architecture by emulating the box's CPU
  (`tonistiigi/binfmt` + `docker buildx`), which compiles Rust under emulation — an hour or more,
  and QEMU's occasional miscompiles (illegal-instruction, atomics) made it flaky as well as slow.
  Both Dockerfiles now pin their builder stage to `$BUILDPLATFORM` (the runner's own CPU) and emit
  a `$TARGETPLATFORM` binary: `deploy/Dockerfile` cross-compiles `pos_cloud` for the Rust triple
  chosen from `TARGETARCH`, with the `gcc-aarch64-linux-gnu` toolchain linking `ring`'s C/assembly
  for the arm64 build; `deploy/caddy.Dockerfile` cross-compiles Caddy with `GOARCH`. Both runtime
  stages now run **no** commands (CA roots are `COPY`ed from the builder, the app runs as a numeric
  uid), so no stage of either image is ever emulated — the build is entirely QEMU-free and runs at
  native speed. The workflow drops the `binfmt` install and lowers its timeout from 120 to 60 min.
  The box-architecture detection over SSH is unchanged. No image contents change for an amd64 box.

### Fixed
- **The arm64 cross-compile now has the target libc headers, so `ring` compiles.** The builder
  installed `gcc-aarch64-linux-gnu` with `--no-install-recommends`, which drops the *Recommended*
  `libc6-dev-arm64-cross` — the arm64 target C headers. The cross compiler was present but headerless,
  so an arm64 deploy died in the builder with `ring` failing to compile `curve25519.c` ("fatal
  error: … No such file or directory / compilation terminated"). `deploy/Dockerfile` now installs
  `libc6-dev-arm64-cross` alongside the cross gcc. amd64 builds are unaffected.
- **Deploy now builds images for the VPS's own CPU architecture, not the runner's.** The `deploy`
  workflow always produced amd64 images (`docker build`, `docker pull`), so on an arm64 box — Oracle
  Ampere, AWS Graviton, most free-tier ARM instances — every built/pulled container died with
  `exec /usr/bin/caddy: exec format error` / `Restarting (255)`, and nothing listened on 80/443 (the
  multi-arch official postgres/nats/garage images came up, masking it). The workflow now detects the
  box architecture over SSH (`uname -m` → `linux/amd64` | `linux/arm64`) before building, then builds
  `pos_cloud` and the custom Caddy image with `docker buildx --platform <target> --load` (QEMU via
  `tonistiigi/binfmt` for the non-native arch) and pulls the stock Caddy image with `--platform`. The
  job timeout rises to 120 min because a cross-arch Rust build under emulation is slow. Unsupported
  `uname -m` values fail fast with a clear message.
- `deploy/reset-admin.sh`: the break-glass **no longer errors when the admin tables do not exist
  yet.** It ran `DELETE FROM super_admin; DELETE FROM admin_sessions;` unconditionally, so
  `reset_admin=true` on a first deploy (before the app's first migration created those tables) failed
  the workflow with `relation "super_admin" does not exist` — contradicting the script's own "idempotent
  … still succeeds" promise. Each DELETE is now guarded by `to_regclass`, so a reset before the schema
  exists is a clean no-op. (`reset_admin` is only meant for wiping an *existing* super-admin; a first
  deploy should leave it off and enrol via the setup token.)
- `deploy/bootstrap.sh`: **`cloud.toml` is now readable by the app container on a non-root deploy
  user.** `pos_cloud` runs as uid 10001 and its config is a mode-600 file; bootstrap only `chown`ed it
  when run as root, so a sudo user (the common cloud default — Oracle's `ubuntu`, etc.) left the file
  owned by the deploy user and the container could not read it, so `pos_cloud` would fail to start.
  bootstrap now falls back to `sudo -n chown` when not root (no regression where neither root nor
  passwordless sudo is available — still a warning, not a hard failure). The one-time super-admin
  setup token is captured at cloud.toml creation and printed from that variable, so it still shows
  even after the file is chowned away from the deploy user (previously it was re-read from the file
  afterward, which the chown could make unreadable). Only `pos_cloud` needs this — `postgres` reads
  `pos.env` via the daemon (`env_file`) and the `nats`/`garage` images read their mounts as root.
- **Deploy was broken on the sslip.io path (two ways).** (1) The `deploy` workflow always built a
  custom Caddy image via `xcaddy --with caddy-dns/cloudflare`; against a current plugin that build
  failed on Caddy 2.8.4 with `undefined: zapslog.HandlerOptions` (xcaddy resolved `go.uber.org/zap`
  past the experimental API 2.8.4 references), so *no* deploy could produce images. (2) Even had it
  built, `deploy/Caddyfile` hard-coded a Cloudflare DNS-01 `tls{}` block, so an sslip.io deploy (empty
  `CF_DNS_API_TOKEN`) would fail certificate issuance and never serve `:443` — while the docs promised
  sslip.io "just works". Fixed both: the Caddy image and Caddyfile are now chosen by `DOMAIN` —
  `*.sslip.io` uses the **stock official `caddy:2.8.4`** image and an **HTTP-01/TLS-ALPN** `Caddyfile`
  (no plugin, no Cloudflare, the fragile `xcaddy` build skipped entirely), while a managed domain keeps
  the custom image (now pinned to Caddy `2.10.0`, which builds the plugin against a self-consistent
  module graph) and the DNS-01 `deploy/Caddyfile.cloudflare`. `bootstrap.sh` selects the Caddyfile from
  `DOMAIN` (read back from `caddy.env`, so re-runs are correct); the workflow selects the image and
  carries it as `CADDY_IMAGE`. Docs updated to state both modes. The custom-image (domain) build runs
  only in the manual deploy workflow, so confirm it against a real Cloudflare domain when first used.
- `store-postgres` integration test (`subjects_store::fetch_due_then_mask_is_idempotent`): the seed
  bound a `&str` to a `jsonb` column via `$4::jsonb`, which makes Postgres infer the *parameter* as
  `jsonb` and `tokio-postgres` then rejects the `&str` (`WrongType { Jsonb, "&str" }`) — the seed never
  ran, failing the merge-to-main integration lane against a live PostgreSQL. Changed it to `$4::text::jsonb`
  (pin the parameter to `text`, cast to `jsonb` in the database), the exact idiom every production writer
  already uses — `subjects.rs` (`mask`), `config_trees.rs`, `translations.rs`, `rollups.rs`. Test-only;
  no shipped code changed. The PR-gate CI does not run the Postgres lane, so this surfaced only on merge.
- `justfile` (P13): **`just` was completely broken** — a stale "Development loops" block left duplicate
  `run-edge`/`simulate` recipes and placeholder `run-cloud`/`deploy` bodies, and with no
  `allow-duplicate-recipes` setting `just` refused to parse the file at all, so *every* `just` command
  failed. Removed the duplicates, gave `run-cloud` a real body (it runs `pos-cloud` against a
  `POS_CLOUD_CONFIG` path and names the backends it needs), and replaced the stale `deploy` placeholder
  with a pointer to the deploy workflow and runbook. Found by walking the P13 fork-to-UI checklist.
- `README.md` (P13): the Quickstart listed `VPS_SSH_PORT` (a secret the deploy workflow does not use) and
  omitted `VPS_KNOWN_HOSTS` (one it requires). Rewrote it to run `just run-edge` locally and defer the
  deploy secrets to the single source of truth in [`docs/deploy-runbook.md`](docs/deploy-runbook.md),
  which now leads with the six-secret sslip.io fastest path, so a first-time reader cannot copy a wrong
  secret list. Added `docs/guides/` and `docs/deploy-runbook.md` to the documentation map.
- `pos-core` tests (P9): **OTA rollout scenarios** over a virtual fleet
  (`crates/pos-core/tests/ota_rollout.rs`), the `docs/roadmap.md` P9 exit proof seeded against the pure
  `decide_rollout` ahead of P12's full `pos-simulator`. Five scenarios pin the safety properties with no
  I/O or clock: the fleet canary ramps by bucket while lab and pilot take the update immediately; a
  raised minimum ring holds the lower rings back; a failed self-test rolls back over the kill switch and
  a revoked key both; the kill switch freezes an otherwise-eligible fleet; and a revoked signing key is
  refused fleet-wide even at full ramp.
- `examples/minimal-edge`: the smallest runnable store — `pos_edge` on a fixed dev store id with no
  database, hardware, or config file. `just run-edge` runs it; it grows to compose the edge over
  `pos-fakes` as the P5 domain routes land.
- `cargo-deny` gains two curated, dated `skip`/`skip-tree` entries (`syn@2`, `sha2@0.11`): transient
  duplicates from the ecosystem's mid-migration across major versions, all build-time or
  handshake-only and none changing what a shipped binary links. This is the curation the `deny.toml`
  comment anticipated for when the axum/tokio stack arrived; both entries are reviewed on 2026-11-19.

- `pos-core` permission registry (`pos-spec.md` §9): a **fixed catalogue** of 24 permissions declared
  through one `permissions!` macro, so a new permission is a single entry that cannot omit its group,
  risk, PIN flag, default roles or description — the enum, its `ALL`, and `Permission::meta` all
  derive from that one block and cannot drift. `PermissionSet` is a `u64` bitset (a compile-time
  assertion keeps the catalogue inside 64), roles are data synced from the cloud, and every check
  goes through one `require()` gate that is **deny by default** and returns `DomainError::PermissionDenied`
  naming the id. High-risk money vectors (void, comp, refund, price override, drawer-no-sale) carry a
  mandatory PIN flag a test enforces. `docs/snapshots/permissions.txt` records every id under the same
  removal gate as the event catalogue — the bare id is an immutable contract, the tabbed metadata is
  mutable — and `docs/permissions.md` is the generated role matrix.

- `pos-core` capability context (`pos-spec.md` §10): the ten store-profile flags (`tables_enabled`,
  `kds_enabled`, `pay_first_enabled`, …) as a fixed catalogue declared through one `capabilities!`
  macro, read through a single `CapabilityContext` — `require()` returns `DomainError::CapabilityDisabled`
  naming the key, so the banned "scatter `if flag` through the code" pattern has nowhere to live.
  Full-service, cafe-counter and retail are three presets over the same flags. Inter-flag validity
  (`pay_first` excludes `tables`; `seats` requires `tables`) is `conflicts()`, a pure function over
  enumerable `RULES` the cloud runs before publishing a config version and the edge could run
  identically. `docs/snapshots/capabilities.txt` puts every flag key under the removal gate — a key
  is a config term a synced edge reads — with its `default` as mutable metadata.

- `pos-core` business-date derivation (`pos-spec.md` §14.1, ADR-0014): `derive_business_date` turns
  an instant into the trading day it belongs to, in the **store's** timezone with its cutoff hour
  (default 04:00) — computing rollups in the server's timezone is named in `docs/roadmap.md` P3 as
  *the* classic revenue-skewing bug. It runs the safe direction (instant → civil) and subtracts the
  cutoff as civil arithmetic, so a 25-hour fall-back day needs no special case. `resolve_local_time`
  handles the ambiguous direction (daypart and shift boundaries) with the one policy ADR-0014 fixes —
  a skipped local time resolves forward, a doubled one to the earlier instant. `StoreTimeZone` and
  `CutoffHour` validate at construction so a bad IANA name or hour fails once, not per derivation, and
  `jiff` stays out of the crate's public signatures. Tests cover Ho Chi Minh, Honolulu, and both US
  DST transitions. `pos-core` now enables `jiff`'s `tzdb-bundle-always` feature (ADR-0014), so the
  timezone database is compiled into the binary as pure data — Windows ships none and the edge is a
  static binary on an unadministered machine; this adds to binary size on both tiers, accepted
  because the tablet and the cloud aggregator must apply the *same* rules or one bill lands on two
  different business dates.

- `pos-core` inventory (`pos-spec.md` §8): recipes as a **bill of materials per item and per
  modifier** (a modifier carries its own `MenuItemId` and its own `Recipe`, so "the large size adds
  50 g of dough" is a recipe like any other), a `StockProjection` of on-hand quantities updated by
  the five ledger movements, and `available(item) = floor(min over ingredients(on_hand / per_unit))`.
  Because availability reads the current projection every time, shared ingredients propagate for
  free — the archive's C=10/D=8/E=6 fixture is a test: cooking one A drops B's availability from 8 to
  7 through the shared ingredient D, without B being sold. `Availability::is_sellable` is the auto-86
  decision; `consumption_for_fire` sums base plus modifiers scaled by line quantity; and
  `stocktake_movement` computes the delta against the projection **at count time**, so sales during a
  count are preserved rather than overwritten. All arithmetic is integer `Quantity` in thousandths —
  no float on the availability path.

- `pos-core` campaign engine (`pos-spec.md` §7): one `Campaign` model for happy hours, item and
  category discounts, combos, vouchers and manual reductions, evaluated in §7's **deterministic
  order** (item-level → combo → bill-level → voucher → manual, then by descending priority, then by
  id) with a **split timing** — `evaluate` at `Timing::LineAdd` applies item and combo rules against
  the line, at `Timing::PaymentStart` the bill-level and voucher rules against the bill, so a guest
  who ordered at 16:59 keeps the happy-hour price when they pay at 17:30. The voucher stage is
  skipped entirely when `Connectivity::Offline` — rules run offline, uniqueness runs online.
  Exclusion groups admit only their highest-priority match, quota gates each campaign, schedule
  windows may wrap past midnight, and each applied campaign is its own reduction line computed on the
  running remainder so the total can never exceed the base. Percentage and fixed-amount actions are
  modeled; combo-price and free-item actions wait for the `decide` slice's menu/line model.

- `pos-core` decision spine (`decide(state, command, ctx) -> Decision`, ADR-0013): the sans-I/O
  point where a command meets the domain. `DecisionCtx` is the single place a decision reads ambient
  truth — `now` (read once, a value not a clock), the derived `business_date`, the actor, the granted
  `PermissionSet`, the `CapabilityContext`, connectivity and currency — so "the clock is read once"
  and "flags are read through one surface" are structural, not conventional. `decide_line` wires the
  order-line command family through the state machine (legal transitions), the permission registry (a
  void-after-fire needs `sales.line.void_fired` **and**, because it is PIN-flagged, a verified PIN),
  the capability profile (firing by course needs `courses_enabled`), and inventory (a fire's
  consumption movements). It returns a `#[must_use] LineDecision` carrying the next state, the stock
  ledger writes, and the post-commit `Effect`s (print a void ticket, recheck availability).

- `pos-core` decision spine — the remaining command families. `decide_bill` settles a bill through
  `billing::settle` (the invariant is proven or the command refused) and voids one behind
  `billing.bill.void` + PIN; `decide_shift` runs the **blind** close (§11.1) — the count is recorded
  without the decision revealing the expected total — behind `cash.shift.close`; `decide_table`
  drives the floor cycle (seat → request bill → settle → clean) gated wholesale by the
  `tables_enabled` capability. All four aggregates (line, bill, shift, table) now share one
  `DecisionCtx`/`Effect` spine, so the `decide(state, command, ctx) -> Decision` orchestration is
  complete across the P3 lifecycles.

### Changed
- `README.md`'s repository layout moves country modules out of `crates/adapters/` and up to
  `countries/` at the root. Filing `fiscal-vn` beside `store-sqlite` described a country as one
  implementation of one port when it is five things, and it hid the unit a fork adds or removes.
- `pos-spec.md`: tax is per item class and keyed by sales channel, not a flat store rate;
  a table has exactly one open order; one open shift per cashier device; queue numbers
  reset daily and are not the receipt counter.
- `naming-and-api.md`: the `bills:split` and `webhook_deliveries:redeliver` custom methods.
- Cargo workspace with the three backbone crates, the pinned toolchain, layered
  lints, `deny.toml`, the `justfile`, and the `xtask` crate carrying the repository
  checks: the dependency rule, the per-crate `clippy.toml` baseline guard, action
  pinning, and internal documentation links. Each is proven to fire, not merely
  written.
- CI: a pull-request gate under ten minutes (rules, lints, tests, both build
  targets, licences, secrets, changelog), a merge-to-`main` workflow, a nightly
  advisory scan, and a daily mirror with a deletion-proof bundle.
- `pos-proto` value types, the foundation every later calculation trusts: `Ulid`
  (in-house Crockford base32, injective, time-sortable), `Money` with `Ratio`,
  `Quantity` and a single `div_round` primitive, `Timestamp`, and `BusinessDate` and
  `CalendarDate` as deliberately unconvertible types. Eighteen resource-identifier
  newtypes over `Ulid`, so a `StoreId` cannot be passed where a `TenantId` belongs.
  Fifty-six tests including property tests for the split-rounding law.
- `pos-proto` wire machinery: `Open<E>`, which degrades an unknown enum value to
  `*_UNSPECIFIED` **while retaining the original token**, plus `require()` as the
  domain boundary that refuses it; the `wire_enum!` macro; ten closed vocabularies
  (`OrderState`, `PaymentMethod`, `PaymentOutcome`, `ReductionKind`, …); `NoPii` as a
  sealed marker so text in an event payload is a compile error with an instruction
  attached; and the two determinism traits, `ClockSource` and `IdGenerator`.
- The event envelope, the AIP-193 error envelope with its nine canonical statuses, the
  `PROTOCOL_VERSION` handshake, and the full event catalogue: **49 types**, being the 38
  the specification declares plus 11 that stated rules needed and nothing carried.
- `docs/snapshots/events.txt`, generated from the catalogue, with a CI gate that refuses
  any removal — a published event type or payload field is a contract.
- Four narrow text types (`DisplayName`, `TranslationKey`, `PermissionKey`,
  `ReleaseTag`), each admissible in an event payload for a stated reason, and
  `GuestNote`, which deliberately is not.

### Changed
- **`pos-spec.md` §18 now lists 49 event types.** The eleven additions are tabulated with
  the rule each one serves; the sharpest is `security.permission.overridden`, since a
  manager-PIN override above a discount ceiling is a named fraud control that had no
  auditable record at all.
- **`pos-spec.md` §3: a line note's text never enters the event log.** It is where "for
  Mr Nguyễn, severe peanut allergy" gets typed — a name and a health condition — and the
  log is immutable, so nothing personal in it could ever be erased. Events carry only
  whether a note exists; the kitchen reads it from the local order record.
- Every document now carries the mandatory `Status` / `Owner` / `Last reviewed` header that
  `engineering-guide.md` §12b requires.
- `architecture.md` §5 is now the authoritative port table and lists **sixteen** ports.
- `engineering-guide.md` §8's ADR index reached only 0009; it now covers every record.

### Fixed
- **The dependency rule reported crates that are never linked.** Reading
  `cargo metadata`'s resolve graph reported `log`, `defmt` and `bitflags` behind
  `jiff`, none of which this workspace activates, and reading `cargo tree` instead
  reported `syn` and `quote`, which run inside the compiler. The check now uses the
  metadata graph for structure, follows an edge only when `cargo tree` says it is
  activated, and stops at procedural macros — so the allow-list stays a statement
  about runtime dependencies rather than accumulating build-time noise. Two tests
  pin both halves.
- **`OrderIn` was missing from the port list.** ADR-0006 and `architecture.md` §5 both named
  fifteen ports and omitted it, although ADR-0012 and `pos-spec.md` §13 depend on it — it is
  the reason QR ordering reuses the marketplace intake path instead of adding a pipeline.
  ADR-0021 supersedes ADR-0006 with the corrected list.

### Upgrade notes
- Documentation and decisions only; no code, no protocol, no migrations, no permission
  changes. ADR-0006 is marked superseded rather than edited — its decision stands, only its
  port list was incomplete.
- The permission catalogue introduces 24 permission identifiers (`docs/snapshots/permissions.txt`).
  This is the initial catalogue, not a change to an existing one, so nothing needs migrating; but
  from here on adding, retiring or re-defaulting a permission is an `Upgrade note` under rule 4, and a
  role synced from an older cloud that still names a retired id must keep resolving — ids are
  deprecated, never removed.
- `CODEOWNERS` routes review to four `@maintainers-*` teams. GitHub **silently ignores** an
  entry naming a team that does not exist, so the required-review protection on the backbone
  crates does nothing until those teams are created. See `MAINTAINERS.md`.

---

## Template for a released version

```markdown
## [1.4.0] — 2026-09-01

**Product version** 1.4.0 · **Protocol version** 3 · **MSRV** 1.83
**For restaurant staff:** split bills now always add up to the original total; nothing else changes on screen.

### Added
- Seat-level ordering behind the `seats_enabled` capability flag. (#204)

### Fixed
- Rounding remainder on uneven bill splits is assigned to the final split. (#231)

### Upgrade notes
- Migration `0042_add_seat_to_order_lines` is additive; rollback to 1.3.x is safe.
- New permission `sales.order_line.assign_seat` is granted to the Server template by default.
- No protocol change; cloud 1.4.0 serves edge 1.2.x and 1.3.x.
```
