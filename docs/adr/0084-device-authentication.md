# ADR-0084 — Device authentication: the edge enforces the pairing token on every domain route

**Status** Accepted · **Owner** @maintainers-edge · **Last reviewed** 2026-09-02
**Amends** [ADR-0030](0030-pairing-and-offline-auth.md) (pairing issued a device token; this ADR makes presenting it mandatory) · **Relates to** [ADR-0018](0018-http-websocket-stack.md) (the HTTP/WS surface this gates) · `docs/roadmap-v3.md` (roadmap v3, slice S0 — the security fix that gates the pilot)

**Context.** ADR-0030 built device pairing: a tablet reads a six-digit code off the edge's console and redeems it for an opaque device token. But nothing ever checked that token. Every domain route resolved a **fixed** actor — `dev_actor()`, hardcoded to employee 1 / device 1 — regardless of who called, and the WebSocket fan-out and the domain routes accepted any caller on the store LAN. The consequence, found in the roadmap-v3 audit: **any host on the store network could seat tables, settle bills, close shifts, and read the floor**, all recorded as employee 1. A pairing token was minted and thrown away; the permission model, the audit trail, and the fraud controls the later waves build all rest on an identity that was forged. Nothing downstream is meaningful until a command is known to come from a device the store admitted.

**Decision.** The edge **requires a valid device token on every domain route**, and resolves the acting device from it.

1. **The token binds to a device.** `Pairing` now records each issued `DeviceToken` against a freshly minted `DeviceId` (`redeem` mints it; `device_for(&token)` resolves it). A token is 32 hex characters; `DeviceToken::parse` rejects anything else before a lookup.

2. **A middleware gates the whole domain router.** `require_paired_device` reads `Authorization: Bearer <token>`, validates it against `Pairing`, and either places the resolved `DeviceId` in the request extensions or answers `401`. It runs over the pairing state, independent of the routes' own state, so it guards **reads and writes alike** — an unpaired device has no more business reading the store's tables than commanding them. Absent, malformed, and unknown tokens get the same `401`, so a probe learns nothing.

3. **The actor carries the real device.** Each command handler builds its `Actor` from the extension `DeviceId` (`device_actor`) instead of `dev_actor()`, which is deleted. The operator UI stores the token it receives on pairing and sends it on every call; an unpaired device is routed to the pairing screen, and a `401` clears the stored token so a stale one re-pairs.

**Deliberately deferred (flagged, not silently dropped).**

- **~~The employee is still a placeholder.~~** *Resolved by S0b — see the amendment below.* This slice (S0a) authenticated the *device*; the *employee* an action runs as was a fixed placeholder until PIN sign-in resolved a real one.
- **~~`/ws` is not yet gated.~~** *Resolved by S0c — see the second amendment below.* Deferring the read-side eavesdrop to the integration slice was the wrong urgency: what `/ws` streams is the store's whole committed-event log, so this was a live exposure, not a missing feature. The browser-side subprotocol handshake it names is exactly how S0c carries the token. Only the *scope* and event-type filter stay with W6·B6.1.
- **Tokens are in memory.** The issued set is not persisted, so an edge restart clears it and every device re-pairs. Persisting it (an edge-local table, or the OS keyring alongside activation) is a follow-up; it is an operability concern, not a security one — enforcement is strictly safer than the prior state either way.

**Consequences.**

- The LAN-command hole is closed: only a device that redeemed a pairing code an operator read off the console can reach the edge, and every action is attributable to that device. This is what makes the permission model, audit trail, and fraud controls of the later waves rest on a real identity rather than a forged one — and what makes a pilot defensible.
- No wire or protocol change to the cloud, no migration, no `pos-proto` change: this is edge HTTP behaviour plus the operator UI's client. The `PairRequest`/`PairAccepted` shapes are unchanged; what changed is that the token they carry is now required. **Upgrade note:** a device must pair before it can use the edge — the operator UI handles this automatically (it routes an unpaired device to the pairing screen), and an existing paired device re-pairs after an edge restart until token persistence lands.

---

## Amendment — S0b: a real employee signs in on the paired device (2026-09-01)

**Context.** S0a closed the LAN-command hole but every action still ran as a placeholder employee: the device was authenticated, the person was not. The roadmap holds S0 to *"a real actor from pairing + PIN sign-in replaces the hardcoded employee"* — S0b resolves the person half so every sale, void and shift is attributable to who did it, which the permission model and fraud controls of the later waves depend on.

**Decision.** A paired device commands nothing until a member of staff **signs in** on it with their badge code and PIN, and the command then runs as that employee.

1. **The roster already carries the identity.** The published `permissions` node (ADR-0070) already sends each staff member's employee `id`; the edge now reads it into `StaffAuth.employee_id` (a member with no id, or no PIN, cannot sign in — the edge never invents an identity to act as). No new `pos-proto` field, no cloud change.

2. **Sign-in over the paired surface.** `POST /api/session/sign-in { code, pin }` verifies the PIN against the synced roster through the existing offline `Lockout` (Argon2id + the five-fail/five-minute rate limit of ADR-0030), and on success binds the device to that employee in an in-memory `Sessions` map. `POST /api/session/sign-out` clears it; `GET /api/session` reports who is signed in so the UI resumes on the right screen after a reload. These session routes sit behind the paired-device gate but not the sign-in gate — signing in is how a device passes it. An unknown code, a member with no PIN, and a wrong PIN answer identically (no `remaining`), so a probe cannot enumerate codes.

3. **A second gate resolves the person.** `require_signed_in` runs after `require_paired_device` on the command-and-read routes: it maps the request's `DeviceId` to the signed-in employee, builds the `Actor` from it, and places it in the extensions for the handlers — which now read `Extension<Actor>` and no longer build one from the device. A paired device with nobody signed in is refused **`403`** (distinct from the unpaired **`401`**), so the UI shows the sign-in screen rather than sending the operator back to pair. `device_actor` and its `UNASSIGNED_EMPLOYEE` placeholder are deleted.

4. **The operator UI.** After pairing, the device goes to a sign-in screen (staff code + PIN), not straight to the floor; on a `403` from any command the app routes there; a sign-out control on the status bar ends the shift. The PIN is sent once and never stored; the device token persists as before.

**Enforcement posture.** Sign-in is **mandatory** (chosen over an additive placeholder fallback): the command routes reject until a real employee is signed in, matching the roadmap's *"identity is forged is unacceptable"*. There is no capability flag to relax it in this slice.

**Deliberately deferred (flagged).**

- **Authority still comes from the store default, not the person.** S0b fixes *who* a command runs as (the `Actor`'s employee); *what* they may do still reads the store-level granted set, not the signed-in employee's own `PermissionSet` (which the roster now carries but the decision path does not yet consult). Per-actor permission enforcement lands with the capability-enforcement pass (roadmap-v3 B2.3 / B5.3).
- **Sign-in is device-local and in memory.** The binding lives beside the pairing tokens; an edge restart clears it and staff sign in again. Persisting it and emitting a durable `security.*` sign-in event (today the outcome is a `tracing` line, employee id only, never the PIN) are follow-ups.
- **~~`/ws` read-auth~~** ~~remains as S0a left it (roadmap-v3 B6.1).~~ *Resolved by S0c — see the amendment below.* A device that has paired reaches `/ws`; narrowing what a *consumer* may read there is still B6.1.

---

## Amendment — S0c: `/ws` requires the paired device token (2026-09-02)

**Context.** This record deferred the `/ws` gate to W6·B6.1 as "the read-side eavesdrop", grouped with
the scope and event-type filtering a third-party KDS integration needs. That grouping was wrong about
the *urgency*, not the *work*: `/ws` streams every committed event the store produces — orders, bills,
settlements, shift closes — to any host that can route to the box. A laptop plugged into the store
switch read the whole trading day with no command sent and no token presented, which is a live data
exposure, not a missing integration feature. The 2026-09-02 tree audit reclassified it and roadmap v3
pulled it forward as **S0c**. The read-only *scope* and the per-event-type filter stay with B6.1 —
those are what an integration needs; this is what a store needs closed.

**Decision.** `/ws` sits behind the paired-device gate, on the same terms as the domain routes: an
absent, malformed, or unknown token is `401` before the upgrade, and a refused connection never
subscribes to the fan-out.

1. **A sub-router carries the gate, not the whole infra router.** `router()` builds a one-route
   `Router` for `/ws` with `require_paired_device_ws` layered on it and merges that in. The other
   infra routes must stay open: `/healthz` answers an unauthenticated probe, `/api/pair` is how a
   device *gets* a token, and the asset fallback serves the app that does the pairing. The middleware
   runs over `AppState`'s `pairing`, so no signature changed.

2. **The token reaches the gate through `Sec-WebSocket-Protocol`.** The browser `WebSocket` API
   cannot set request headers, so `Authorization` is not available to the store UI on an upgrade. The
   client offers `[<name>, <token>]` and the server selects **only the name** (`pos-edge.v1`), so the
   credential never travels back in the handshake response. Picking the token out of the offered list
   is unambiguous by shape: `DeviceToken::parse` accepts exactly 32 lowercase hex characters, which no
   protocol name is.

3. **`Authorization` still works, and is tried first.** A non-browser consumer — the third-party KDS
   B6.1 anticipates, or a script — can set the header and should not have to learn the workaround. A
   client that offers no subprotocol negotiates none, which RFC 6455 permits.

**Rejected: the token as a query parameter** (`/ws?token=…`). It is the common workaround and it is
wrong here for a reason this repository has already committed to: the edge logs the request path on
every request, so the token would be written into a log — the exact "no secret in a log" rule
`http/auth.rs` states, and one careless retention setting away from being permanent. A header is not
logged, and a subprotocol is a header.

**Consequences.**

- The read-side exposure is closed. The fan-out tests now pair before connecting, and three new cases
  prove the refusals: an unpaired host, a well-formed token that was never issued, and the bearer
  path a non-browser consumer uses. Before this slice those same fan-out tests passed *without* a
  token — the fan-out was proven to work and never proven to be closed, which is how the hole
  survived two auth slices.
- **Upgrade note:** an operator UI older than this edge cannot open `/ws` (it sends no subprotocol and
  no header), so it will show as disconnected and fall back to its polling path. The UI and the edge
  ship together in one OTA artifact, so this only bites a hand-mixed pair.
- The in-memory token set of the deferral above still applies, and now reaches further: an edge
  restart drops every `/ws` connection until each device re-pairs. That is **S0d**.
