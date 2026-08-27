# ADR-0072 — Floor & kitchen: areas/tables and stations as published master data the edge reads

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-27
**Relates to** [ADR-0033](0033-config-tree.md) (config tree) · [ADR-0039](0039-config-delivery.md) (config delivery) · [ADR-0063](0063-store-menu-catalog.md) (the menu catalog this mirrors) · [ADR-0066](0066-cloud-catalog.md) (display/layout plan) · [ADR-0057](0057-qr-ordering.md) (signed table token) · [ADR-0026](0026-port-shapes.md) (`PrintJob` is addressed by `station_id`) · `pos-spec.md` §10 (capabilities), §14 (dine-in) · `docs/cloud-admin-ux-plan.md` (Track M2)

**Context.** A store's **floor** (its areas and tables) and its **kitchen** (its stations and how fired
lines route to them) are operational master data that today exist nowhere the cloud can author or the
edge can read.

1. **Neither the cloud nor the edge has a floor or station roster.** `TableId`/`StationId` are
   identifiers on events, but nothing enumerates *which* tables or stations a store has. The in-store
   app (`ui/`) papers over the gap with two hardcoded constants — an eight-table `FLOOR` and a single
   station `S01` (`ui/src/state/store.ts`) — each carrying a "until the store's real layout syncs from
   config" comment. So the floor is fiction, every store shows the same eight tables, and every fired
   line routes to one imaginary station.
2. **Fired lines trust the client for their station.** `Edge::fire_line` takes a `station_id`
   *parameter*, filled verbatim from the HTTP request, which the UI fills from the hardcoded `S01`.
   There is no item→station routing: the store cannot say "pizzas go to the oven, drinks to the bar".
3. **`mint_table_token` is orphaned.** The QR-ordering path (ADR-0057) *verifies* a signed table token
   on every guest order (`verify_table_token`, live in `qr_http.rs`), but the function that *mints*
   one has no route and no caller — an operator has no way to produce the QR a guest scans.

**Decision.** Model the floor and kitchen as tenant-authored master data, compile each to a structured
configuration node, publish it through the config tree, and have the edge read it — the exact pattern
the menu (ADR-0063) and permissions (ADR-0070) already use, so nothing new is invented.

- **Two shared plan types in `pos-proto`, read by both cloud and edge.** `FloorPlan` (areas, each with
  its tables — a table carries a label, a seat count, and an optional grid position for the visual
  editor) and `StationPlan` (kitchen stations, each with an optional backup station, plus the
  item→station routing rules). They mirror `MenuBook`: lists (not maps) so they round-trip in a diff,
  forward-compatible serde, and defined in `pos-proto` because they cross the wire and both sides read
  them through one type — the cloud and the edge cannot disagree on what a floor *is*. A new `AreaId`
  newtype (the `resource_id!` macro) joins the existing `TableId`/`StationId`.
- **The edge applies `floor` and `stations` on every config pull.** `session_from_config` gains a
  branch per node with the same **never-blank** gate the `menu`/`permissions`/capability branches keep:
  parse the node; replace the session's plan only if it parses; a pull that says nothing about the
  floor never blanks a trading store's tables. `EdgeSession` gains `floor`/`stations` fields and
  `with_floor`/`with_stations` builders.
- **The edge resolves the station itself.** A pure resolver over `StationPlan.routing` turns a fired
  line's item (and its course) into the `StationId` it belongs to, falling to a default station when no
  rule matches. The fire path derives the station from the published plan instead of trusting the
  client — the routing rules finally have something to stand on, and `S01` stops being authoritative.
  A station's `backup_station_id` is the failover target a print dispatcher consults when the primary
  station's printer is down; the authored mapping and the resolver are delivered here. Driving a live
  ESC/POS printer from the edge binary is unchanged (it remains unwired, a hardware-gated integration
  on Track A) — `PrintJob` is already addressed by `station_id` (ADR-0026), so the routing this ADR
  publishes is exactly what a dispatcher will consume when the hardware lane lands.
- **Table QR minting gets a route.** A console route wires the orphaned `mint_table_token` (the signing
  secret is already in cloud config) to produce, per table, the signed token and the guest URL, so the
  console can render a printable QR sheet. The token binds `(tenant, store, table)` only — no personal
  data — so it is not T1; it is the same token `verify_table_token` already checks.
- **A form-driven console, not JSON.** New Floor (areas/tables, with a visual drag-drop grid) and
  Stations (stations + routing + backup) screens author the data, preview a diff, and publish — the
  master-data console the rest of Track M builds. Publishing merges the `floor`/`stations` keys into
  the store's config layer (the node-merge the catalog/people publishes use), so the other Store-level
  keys survive.

**Consequences.** A store's real floor and kitchen reach the counter: the eight-table fiction and the
single-station fiction are replaced by published data, and fired lines route by rule. Additive
throughout — two new top-level config nodes, no `PROTOCOL_VERSION` bump (a store on an old build simply
ignores nodes it does not read, and the never-blank gate means an unpublished floor changes nothing).
The QR token minted here is the one already verified, so QR ordering closes end-to-end. The live print
driver stays out of scope (hardware-gated); everything this ADR ships is authored data, a pure
resolver, and the edge reading a config node — all testable without hardware.

**Slices (Track M2, one PR).**

1. **Proto + validation (this):** the `AreaId` newtype, the `FloorPlan`/`StationPlan` shared types, and
   the pure referential checks (a routing rule names a known station; a backup names another station),
   with round-trip and snapshot tests.
2. **Master-data store:** migration 0025 (areas, tables, stations, routing rules) on the registry
   template; `FloorStore`/`StationStore` seams + store-postgres adapter + fake + tests.
3. **CRUD routes:** `/admin` create/list/update/archive for areas, tables, stations, and routing rules,
   audited, with the typed console client.
4. **Publish:** compile the authoring rows to `FloorPlan`/`StationPlan` and publish the `floor`/
   `stations` config nodes (Store-layer merge + version), mirroring the catalog/people publishes.
5. **Edge applies:** the `session_from_config` `floor`/`stations` branches, the `EdgeSession`
   fields/builders, the station-routing resolver feeding the fire path, and an edge read endpoint the
   in-store app pulls.
6. **QR minting:** the table-token minting route + a printable QR sheet in the console.
7. **Console screens:** the Floor visual editor and the Stations editor, with a pointer-drag primitive
   (the kit ships only a keyboard reorder today), diff-before-publish, and i18n.
8. **Kill the fiction:** `ui/` reads the synced floor/stations, dropping the hardcoded `FLOOR`/`S01`.
