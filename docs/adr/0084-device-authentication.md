# ADR-0084 — Device authentication: the edge enforces the pairing token on every domain route

**Status** Accepted · **Owner** @maintainers-edge · **Last reviewed** 2026-09-01
**Amends** [ADR-0030](0030-pairing-and-offline-auth.md) (pairing issued a device token; this ADR makes presenting it mandatory) · **Relates to** [ADR-0018](0018-http-websocket-stack.md) (the HTTP/WS surface this gates) · `docs/roadmap-v3.md` (roadmap v3, slice S0 — the security fix that gates the pilot)

**Context.** ADR-0030 built device pairing: a tablet reads a six-digit code off the edge's console and redeems it for an opaque device token. But nothing ever checked that token. Every domain route resolved a **fixed** actor — `dev_actor()`, hardcoded to employee 1 / device 1 — regardless of who called, and the WebSocket fan-out and the domain routes accepted any caller on the store LAN. The consequence, found in the roadmap-v3 audit: **any host on the store network could seat tables, settle bills, close shifts, and read the floor**, all recorded as employee 1. A pairing token was minted and thrown away; the permission model, the audit trail, and the fraud controls the later waves build all rest on an identity that was forged. Nothing downstream is meaningful until a command is known to come from a device the store admitted.

**Decision.** The edge **requires a valid device token on every domain route**, and resolves the acting device from it.

1. **The token binds to a device.** `Pairing` now records each issued `DeviceToken` against a freshly minted `DeviceId` (`redeem` mints it; `device_for(&token)` resolves it). A token is 32 hex characters; `DeviceToken::parse` rejects anything else before a lookup.

2. **A middleware gates the whole domain router.** `require_paired_device` reads `Authorization: Bearer <token>`, validates it against `Pairing`, and either places the resolved `DeviceId` in the request extensions or answers `401`. It runs over the pairing state, independent of the routes' own state, so it guards **reads and writes alike** — an unpaired device has no more business reading the store's tables than commanding them. Absent, malformed, and unknown tokens get the same `401`, so a probe learns nothing.

3. **The actor carries the real device.** Each command handler builds its `Actor` from the extension `DeviceId` (`device_actor`) instead of `dev_actor()`, which is deleted. The operator UI stores the token it receives on pairing and sends it on every call; an unpaired device is routed to the pairing screen, and a `401` clears the stored token so a stale one re-pairs.

**Deliberately deferred (flagged, not silently dropped).**

- **~~The employee is still a placeholder.~~** *Resolved by S0b — see the amendment below.* This slice (S0a) authenticated the *device*; the *employee* an action runs as was a fixed placeholder until PIN sign-in resolved a real one.
- **`/ws` is not yet gated.** Closing the read-side eavesdrop on the WebSocket fan-out needs the browser-side subprotocol handshake and lands with the slice that owns `/ws` auth, scope, and event-type filtering (roadmap-v3 W6·B6.1). This ADR gates the command surface — the injection hole — now.
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
- **`/ws` read-auth** remains as S0a left it (roadmap-v3 B6.1).
