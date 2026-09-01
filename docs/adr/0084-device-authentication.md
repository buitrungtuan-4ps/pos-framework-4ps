# ADR-0084 — Device authentication: the edge enforces the pairing token on every domain route

**Status** Accepted · **Owner** @maintainers-edge · **Last reviewed** 2026-09-01
**Amends** [ADR-0030](0030-pairing-and-offline-auth.md) (pairing issued a device token; this ADR makes presenting it mandatory) · **Relates to** [ADR-0018](0018-http-websocket-stack.md) (the HTTP/WS surface this gates) · `docs/roadmap-v3.md` (roadmap v3, slice S0 — the security fix that gates the pilot)

**Context.** ADR-0030 built device pairing: a tablet reads a six-digit code off the edge's console and redeems it for an opaque device token. But nothing ever checked that token. Every domain route resolved a **fixed** actor — `dev_actor()`, hardcoded to employee 1 / device 1 — regardless of who called, and the WebSocket fan-out and the domain routes accepted any caller on the store LAN. The consequence, found in the roadmap-v3 audit: **any host on the store network could seat tables, settle bills, close shifts, and read the floor**, all recorded as employee 1. A pairing token was minted and thrown away; the permission model, the audit trail, and the fraud controls the later waves build all rest on an identity that was forged. Nothing downstream is meaningful until a command is known to come from a device the store admitted.

**Decision.** The edge **requires a valid device token on every domain route**, and resolves the acting device from it.

1. **The token binds to a device.** `Pairing` now records each issued `DeviceToken` against a freshly minted `DeviceId` (`redeem` mints it; `device_for(&token)` resolves it). A token is 32 hex characters; `DeviceToken::parse` rejects anything else before a lookup.

2. **A middleware gates the whole domain router.** `require_paired_device` reads `Authorization: Bearer <token>`, validates it against `Pairing`, and either places the resolved `DeviceId` in the request extensions or answers `401`. It runs over the pairing state, independent of the routes' own state, so it guards **reads and writes alike** — an unpaired device has no more business reading the store's tables than commanding them. Absent, malformed, and unknown tokens get the same `401`, so a probe learns nothing.

3. **The actor carries the real device.** Each command handler builds its `Actor` from the extension `DeviceId` (`device_actor`) instead of `dev_actor()`, which is deleted. The operator UI stores the token it receives on pairing and sends it on every call; an unpaired device is routed to the pairing screen, and a `401` clears the stored token so a stale one re-pairs.

**Deliberately deferred (flagged, not silently dropped).**

- **The employee is still a placeholder.** This slice authenticates the *device*; the *employee* an action runs as is a fixed placeholder until PIN sign-in resolves a real one (the offline PIN machinery of ADR-0030 exists but no route calls it yet). Device authenticated, person pending — every command is now attributable to a paired device, and gains a real person on the next slice, on an authenticated foundation. Per-person permissions and the fraud events wait on that.
- **`/ws` is not yet gated.** Closing the read-side eavesdrop on the WebSocket fan-out needs the browser-side subprotocol handshake and lands with the slice that owns `/ws` auth, scope, and event-type filtering (roadmap-v3 W6·B6.1). This ADR gates the command surface — the injection hole — now.
- **Tokens are in memory.** The issued set is not persisted, so an edge restart clears it and every device re-pairs. Persisting it (an edge-local table, or the OS keyring alongside activation) is a follow-up; it is an operability concern, not a security one — enforcement is strictly safer than the prior state either way.

**Consequences.**

- The LAN-command hole is closed: only a device that redeemed a pairing code an operator read off the console can reach the edge, and every action is attributable to that device. This is what makes the permission model, audit trail, and fraud controls of the later waves rest on a real identity rather than a forged one — and what makes a pilot defensible.
- No wire or protocol change to the cloud, no migration, no `pos-proto` change: this is edge HTTP behaviour plus the operator UI's client. The `PairRequest`/`PairAccepted` shapes are unchanged; what changed is that the token they carry is now required. **Upgrade note:** a device must pair before it can use the edge — the operator UI handles this automatically (it routes an unpaired device to the pairing screen), and an existing paired device re-pairs after an edge restart until token persistence lands.
