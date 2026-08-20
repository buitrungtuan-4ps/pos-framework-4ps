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
