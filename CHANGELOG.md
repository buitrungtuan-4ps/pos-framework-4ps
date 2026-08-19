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
