# ADR-0053 — CloudSync: the store's request/response channel to the cloud (the seventeenth port)

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-21
**Relates to** [ADR-0001](0001-offline-first-store-autonomy.md) · [ADR-0006](0006-ports-and-adapters.md) · [ADR-0013](0013-async-strategy.md) · [ADR-0021](0021-corrected-port-list.md) · [ADR-0026](0026-port-shapes.md) · [ADR-0047](0047-minisign-verification.md) · [ADR-0048](0048-ota-rollout-model.md) · [ADR-0050](0050-activation-code-exchange.md) · `docs/architecture.md` §5 · `docs/roadmap.md` P9

**Context.** The sixteen ports ([ADR-0021](0021-corrected-port-list.md)) include `MessageLink`, the
store→cloud channel — but it is deliberately **outbound-only and event-oriented**: the store pushes
events and never blocks on the cloud, which is what lets it sell offline ([ADR-0001](0001-offline-first-store-autonomy.md)).
P9's edge work needs the store to make genuine **request/response** calls to the cloud: exchange a
single-use activation code for its long-lived credential on first boot ([ADR-0050](0050-activation-code-exchange.md)),
and fetch a signed update artifact for an over-the-air rollout ([ADR-0048](0048-ota-rollout-model.md)).
Neither fits `MessageLink`'s fire-and-forget shape, and no other cloud-facing port models a synchronous
ask. `pos-ports` fixes the count at sixteen and requires an ADR to add a seventeenth — this is it.

**Decision.**

- **Add a seventeenth port, `CloudSync`:** the store's request/response channel to the cloud, distinct
  from `MessageLink`. It carries the calls where the store genuinely needs an answer back —
  `activate(code) -> {device_id, credential}` and `fetch_update(release) -> bytes` — and leaves
  `MessageLink` outbound-only, so the offline-first guarantee stays a property of the event pipeline
  rather than something a new method could erode.

- **It is a port, not a binary-internal seam.** Every other cloud boundary is a port with a fake and a
  contract suite ([ADR-0026](0026-port-shapes.md)), and edge→cloud request/response is exactly the kind
  of swappable boundary ports exist for: the transport (HTTP today, whatever a fork needs tomorrow) is
  one adapter, the fake lets the edge's activation and OTA logic be tested with no network, and the
  suite pins the request/response and error semantics every implementation must honour. A parallel
  non-port seam would be a second, untested boundary mechanism for no gain.

- **It names no domain type.** `pos-core` and `pos-ports` are siblings ([ADR-0013](0013-async-strategy.md)),
  so `CloudSync` takes the activation code as a plain `&str` — the edge parses and normalises it with
  `pos_core::activation::ActivationCode` first — and returns the credential as a `Secret` (the
  `KeyVault` type) and the identity as a `pos_proto::DeviceId`. The edge maps to and from `pos-core`
  types at the boundary.

- **Compile-time selected, so no object-safe mirror.** One adapter per binary, like `MessageLink` and
  `KeyVault`, so there is no `DynCloudSync` ([ADR-0013](0013-async-strategy.md)).

- **The transport is not a trust boundary.** `fetch_update` returns the **signed** artifact bytes; the
  edge verifies the minisign signature ([ADR-0047](0047-minisign-verification.md), the `Signer` port)
  before trusting them, so a spoofed or compromised cloud cannot make a store install code. A refused
  activation code surfaces as `permission_denied` with no oracle ([ADR-0050](0050-activation-code-exchange.md)):
  spent, revoked, and unknown are one answer.

**Rejected.**

- **Extending `MessageLink` with request/response methods** — rejected: its contract is "outbound only,
  never wait on the cloud", the foundation of selling offline; a synchronous ask muddies exactly the
  property the offline-first design depends on.
- **A `pos-edge`-internal seam instead of a port** — rejected: it would be the only cloud boundary not
  modelled as a port, with no shared contract suite and no fake, so the edge's activation and OTA logic
  could not be tested without a live cloud, and a fork could not swap the transport the way it swaps
  every other adapter.
- **Two unrelated ports for activation and OTA-fetch** — rejected: both are the same role — a request
  the store makes of its cloud and waits on — so one port with one fake and one suite is less surface
  than two.

**Consequences.**

- `pos-ports` gains `cloud_sync` (`CloudSync`, `ActivationGrant`) — the seventeenth port; `PortName`
  gains `CloudSync`; the crate's "sixteen ports" / "fourteen asynchronous" counts become seventeen and
  fifteen. `pos-contract-tests` gains a `CloudSync` suite (carried by the `SUITES` table and the
  every-port assertion), and `pos-fakes` gains `FakeCloudSync` passing it. No `Dyn` mirror; **no new
  dependency** — the port names only `pos-proto` and its own types.
- [ADR-0021](0021-corrected-port-list.md)'s list and `docs/architecture.md` §5's table gain a
  `CloudSync` row; this ADR is the "adding a seventeenth port still requires an ADR" that both demand.
- Deliberately elsewhere: the real HTTP adapter calling the cloud's `POST /activate` and its artifact
  endpoint (P9e-2b, a new dependency justified under [ADR-0007](0007-in-house-vs-dependency.md)); the
  edge activation client and OTA updater that consume the port (P9e-2d, P9e-4); and P7's configuration
  *pull*, which will reuse this same port.
