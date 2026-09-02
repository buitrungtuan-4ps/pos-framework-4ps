# ADR-0091 — Edge auth state is durable: a `DeviceRegistry` port, hashed tokens, and an idle timeout

**Status** Accepted · **Owner** @maintainers-edge · **Last reviewed** 2026-09-02
**Amends** [ADR-0084](0084-device-authentication.md) (which deferred persisting both tables) · **Relates to** [ADR-0030](0030-pairing-and-offline-auth.md) (the pairing exchange and the offline PIN lockout) · [ADR-0015](0015-sqlite-access.md) (the single-writer store this lands in) · [ADR-0017](0017-migrations.md) (additive-only) · [ADR-0021](0021-corrected-port-list.md) / [ADR-0026](0026-port-shapes.md) (the port list this extends, and the shape it must take) · [ADR-0001](0001-offline-first-store-autonomy.md) (why none of this may need the cloud) · `docs/roadmap-v3.md` (slice S0d)

**Context.** The edge's two auth tables live in process memory. `Pairing` holds
`HashMap<DeviceToken, DeviceId>`; `Sessions` holds `HashMap<DeviceId, EmployeeId>`. Both are
`Mutex`-wrapped fields on structs built fresh by `AppState::new`, so **a restart erases both**.

Every deployment target makes that a service outage, not a theoretical one. The edge runs on a small
box in a restaurant, on mains power, with an OTA updater that is *designed* to restart it
([ADR-0055](0055-edge-ota-updater.md)). So the three ordinary events — a power blip, an OTA install, a
`systemctl restart` — each produce the same Friday-evening scene: every tablet in the store is
suddenly unpaired, and an operator has to walk to the edge's console, read a six-digit code, and pair
each device again, one at a time, while the queue builds. There is no way to avoid it and nothing
warns that it is coming.

[ADR-0084](0084-device-authentication.md) flagged this ("Tokens are in memory… persisting it is a
follow-up") and rated it "an operability concern, not a security one". That rating was right about the
*direction* — enforcing an in-memory token is strictly safer than enforcing nothing — and it
understated the operational cost, which is total loss of device trust on every restart. S0c then made
it worse in one specific way: `/ws` is now gated too, so a restart drops every live socket as well as
every command path.

**Two things have to be decided, not just plumbed.** Persistence is easy; what it *changes* is not.
Today a restart is the de-facto revocation of every token and every sign-in. Making both durable
removes that accidental control, so it has to be replaced with deliberate ones, or the change is a net
weakening dressed as an improvement.

**Decision.**

- **An eighteenth port, `DeviceRegistry`, in `pos-ports`.** Not a choice of taste: every adapter in
  this tree depends on exactly `pos-proto` and `pos-ports` and nothing else (checked in both
  `store-sqlite/Cargo.toml` and `store-postgres/Cargo.toml`), so a trait `store-sqlite` implements
  **must** live in `pos-ports`. Defining it in `pos-edge` is not available — the dependency runs the
  other way. It follows [ADR-0026](0026-port-shapes.md)'s shape: one `PortError`, async methods, and
  `Send + Sync`.

  It is **not** `Transactional`. Unlike [`IntakeLedger`](0064-edge-order-in.md), nothing here must
  commit atomically with an event: pairing a device and signing someone in are not domain events and
  emit none. A crash between issuing a token and storing it leaves the device unpaired, which is the
  safe direction — the operator pairs again — so the extra coupling to `EventStore`'s transaction
  buys nothing and costs the ability to write outside one.

- **The token is stored as a SHA-256 digest, never in the clear.** A device token is a bearer
  credential: whoever reads it *is* that device until it is revoked. Writing 32 hex characters into
  `pos.db` means a stolen till, a copied disk image, or a backup that leaves the store hands over
  working credentials for every device — and a store's SQLite file is far easier to walk off with than
  a running process's heap, which is exactly what changes when this becomes durable.

  A plain digest suffices and a password KDF would be wrong here. The token is 128 bits from the OS
  CSPRNG, so there is no dictionary to run and no salt to add; lookup is by exact equality, so hashing
  the presented token and querying the digest column is the same single indexed read. `DeviceToken`
  keeps its 32-lowercase-hex parse rule, so a malformed token is still refused before any lookup.

- **Revocation becomes explicit, because it stops being accidental.** With tokens durable, nothing
  removes a device's access on its own. So the registry gains a revoke path, and `pos-edge` exposes it
  on the pairing surface: an operator can retire a specific device (a tablet that was lost, sold, or
  replaced) and can retire *all* of them, which is the break-glass that reproduces today's restart
  behaviour on purpose. Revocation is local and works offline — it must, by
  [ADR-0001](0001-offline-first-store-autonomy.md); a store discovering a missing tablet cannot be
  told to wait for the cloud.

- **Sign-in survives a restart too, guarded by an idle timeout.** The alternative — durable pairing,
  volatile sign-in — was considered and is *not* chosen: it is the safer default in isolation, but it
  keeps the half of the outage that lands on staff rather than on the operator, and a mid-service
  reboot would still stop every till at once until every person re-entered a PIN.

  So a device comes back signed in as whoever was signed in, **unless it has been idle past
  `sign_in_idle_timeout`**, in which case the binding is treated as absent and the device gets the same
  `403` an unsigned one gets. The timeout is what makes this defensible: the risk of durable sign-in is
  a till carried off while signed in as a manager, and idleness is the one signal available for that
  without the cloud. Every authenticated request from a device refreshes its `last_seen_at`; the check
  is on read, so an expired binding needs no sweeper to be safe (a background prune is housekeeping,
  not enforcement).

  **Default: 30 minutes**, configurable per store. Long enough that a reboot, a shift-change lull, or a
  quiet hour does not sign anyone out mid-service; short enough that a device that left the store is not
  still trading as that person the next day.

- **Time comes from the edge's `ClockSource` ([ADR-0013](0013-async-strategy.md)), and the timeout
  fails closed when that clock misbehaves.** Worth stating plainly, because it is easy to assume
  otherwise: **no SNTP poll runs today.** `pos-edge`'s `sntp` module is `pub mod` plus two re-exports
  and has no production caller — [ADR-0073](0073-alerting.md) already records the drift signal as
  computed-but-unread with no producer. So the clock behind this timeout is the host OS clock, which
  a system NTP daemon or a person with `date` can step at any moment, forwards or backwards.

  The timeout is therefore evaluated as a difference between two stored instants, and **a negative or
  implausibly large difference expires the binding** rather than extending it: a clock that jumps must
  never be a way to hold a session open. That is the same fail-closed posture
  [ADR-0030](0030-pairing-and-offline-auth.md) took for the PIN lockout, whose stored `until_ms` has
  the mirror-image problem — and it is what makes this decision survive the day an SNTP poll does get
  wired in.

- **One additive migration, `0005_device_registry.sql`**, per [ADR-0017](0017-migrations.md): two
  tables (`paired_devices`, `device_sessions`), nothing renamed, nothing removed. An existing store
  upgrades into it with both tables empty, which is exactly today's post-restart state — so the
  upgrade itself needs no special step and cannot be worse than the status quo.

**Delivery note (2026-09-02, S0d-2).** Implemented, with one decision made during the work and
worth recording because it went the *other* way from the obvious reading of this record.

- **The object-safe view of the port lives in `pos-edge`, not in `pos_ports::dynamic`.** That module
  reserves its mirrors for "the four families that need runtime selection" and states that the other
  ports have none because they are chosen once at startup; `DeviceRegistry` is the second kind, so
  adding it there would have paid `Box::pin` for flexibility nothing uses and diluted a rule the
  module exists to state. The reason a trait object is needed at all is that `pos-edge`'s `AppState`
  is not generic, so the seam belongs where the constraint is — `pos_edge::durable_auth`. `Edge<S>`
  already exposes its store and `serve` already holds an `Arc<Edge<S>>`, so this needed **no new
  parameter to `serve` and no change in `main.rs`**.
- **The in-memory table is keyed by the digest too**, not by the token. It has to be — a restart can
  only restore what was stored — and the consequence is better than the requirement: the edge now
  holds no device token anywhere, in memory or on disk. A token exists for as long as it takes
  `redeem` to hand it to the device that will keep it.
- **`last_seen_at` is flushed at a minute's granularity**, not on every request. The gate touches on
  every authenticated call and the value is only ever compared against a thirty-minute window, so a
  write per request would be a SQLite round-trip on the hot path for nothing. A restart can lose up
  to a minute of freshness, which is immaterial against the window it feeds.
- **Write ordering is deliberate and not uniform.** Pairing and sign-in record durably *before* they
  are believed, so a failure refuses rather than issuing a credential the box may forget. Sign-*out*
  is the opposite: memory clears first and unconditionally, because a half-failed sign-out must leave
  the till locked on this box — an operator told a device is signed out when it is not is the worse
  error. Both are commented at the call site.

**What this does not do.**

- **It does not make the device's identity provable.** A durable token is still a bearer secret: the
  device proves it *holds* the token, not that it *is* that device. Per-device certificates are the
  answer and they belong with the per-store mTLS work [ADR-0089](0089-edge-event-bus-transport.md)
  sequenced as its own slice, because both need an issuing CA and that is a provisioning change, not a
  storage one.
- **It does not change what a device may do.** Authority still reads the store-level granted set, not
  the signed-in employee's own `PermissionSet` — the gap [ADR-0084](0084-device-authentication.md)'s
  S0b amendment recorded, still owned by the capability-enforcement pass (roadmap-v3 B2.3 / B5.3).
- **It does not report to the cloud.** The console cannot list or revoke a store's paired devices from
  the dashboard; that needs the state to reach the cloud, which is a sync change and its own slice.
  Revocation is deliberately local first, so the offline case is the one that definitely works.

**Deliberately deferred (flagged, not silently dropped).**

- **A durable `security.*` sign-in event.** Today a sign-in is a `tracing` line carrying an employee id
  and never a PIN. Making it an event in the log — so the trail survives the box and reaches the cloud
  — is the right end state and is a `pos-proto` catalogue addition, so it is its own change.
- **Pruning.** Expired sessions and revoked tokens are enforced on read and are not swept. A store's
  row counts here are in the tens, so this is untidiness rather than growth; a prune lands with
  whatever first needs to iterate the tables.
- **A store-postgres implementation of the port.** The cloud has no use for it — pairing is a store
  concern — so the port ships with one real adapter and the fake. Adding a second later is additive.
- **Whether the idle timeout should also apply to *pairing*.** A device that has not been seen for
  months is probably gone, and expiring it would close the "lost tablet nobody revoked" case without an
  operator noticing. It is deferred because the right period is a fleet-operations question there is no
  field evidence for yet, and guessing it wrong un-pairs a working till.

**Rejected.**

- **Storing the token in the clear.** Simpler by one hash call, and it turns a stolen `pos.db` into a
  set of live credentials. There is no operational need for the plaintext — nothing ever displays a
  token after issuing it.
- **Argon2id (or any password KDF) on the token.** Consistent with how PINs are stored, and wrong for
  the input: a 128-bit random token has no guessable distribution, and a deliberately slow hash on the
  gate every request runs is a self-inflicted latency budget on the store's hot path.
- **Keeping both tables in memory and only warning louder.** Cheapest, and it leaves the outage in
  place. The roadmap holds S0d to durability, not to documentation.
- **The OS keyring ([`KeyVault`](0086-edge-keyvault-and-activation.md)) as the store.** It already
  holds the device credential and the scoped sync key, so it looked like the natural home. Rejected: a
  keyring is a small set of named secrets, not an indexed table — there is no way to ask it "which
  device does this digest belong to" without reading every entry — and it has no room for the
  `last_seen_at` the timeout needs.
- **Durable pairing with volatile sign-in.** The safer option in isolation and a real candidate. It
  halves the outage instead of ending it: staff still all re-authenticate at once on a mid-service
  reboot. Chosen against deliberately, with the idle timeout carrying the risk that decision creates.
- **A wall-clock session expiry** (sign out at a fixed time, or N hours after sign-in). Simpler than
  idleness and wrong for a restaurant: it signs out the person mid-shift for no reason a user can
  see, and it does not expire the one case that matters — a device that left the store an hour ago is
  still inside its window.

**Consequences.**

- **A restart stops costing the store its device trust.** After this, a power blip or an OTA install
  brings tills back paired and — inside the idle window — still signed in. That is the last of the four
  "restart is a service outage" behaviours the A·P1x audit found on the edge.
- **`pos-ports` grows from seventeen ports to eighteen**, which is a documented port-list change and
  the reason this record exists at all: [ADR-0021](0021-corrected-port-list.md) says in as many words
  that "adding a seventeenth port still requires an ADR first". The bookkeeping follows the pattern
  [ADR-0053](0053-cloud-sync-port.md) set when it added the seventeenth — `docs/architecture.md` §5's
  table (the authoritative one, which today reads *seventeen*) gains a row and a new count, and
  ADR-0021 gains an **amendment note** rather than an edit, because a merged record is immutable. And
  the new port owes a `pos-contract-tests` suite that both `pos-fakes` and `store-sqlite` must pass —
  the rule every port in this tree has followed.
- **A new security boundary exists on disk.** `pos.db` now holds credential material (digests) and
  identity bindings. It holds no PII and no PIN: employee and device identifiers only, which is the
  same class of data the event log already carries. Worth stating for a fork's own review: the file
  was already sensitive, and this is a reason to keep it on an encrypted volume rather than a new one.
- **No wire, `pos-proto`, or protocol change, and no cloud change.** Two additive SQLite tables, one
  new port, and edge-local behaviour.
- **A fork gains one setting**, `sign_in_idle_timeout`, defaulting to 30 minutes. A deployment that
  wants today's behaviour back sets it very low and uses revoke-all after a restart; a deployment that
  wants continuity above all raises it and accepts the stolen-till window that comes with it.
