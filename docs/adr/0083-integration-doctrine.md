# ADR-0083 — Integration doctrine: the core stays small, everything else plugs in through three points

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-01
**Relates to** [ADR-0013](0013-async-strategy.md) (the dependency/no-I/O rule this extends from *layering* to *vendor-neutrality*) · [ADR-0027](0027-country-modules.md) (country modules, the same plug-in shape for jurisdictions) · [ADR-0032](0032-webhooks.md) (webhooks, the event-out point) · [ADR-0056](0056-public-order-intake.md) & [ADR-0061](0061-order-relay.md) (the `/v1/orders` data-in point) · `docs/roadmap-v3.md` (the roadmap whose plug-and-play principle this ADR is the law for)

**Context.** The framework is a *POS core*, not a monolith that grows a branch every time a new
partner appears. A Grab feed, a Shopee feed, an external KDS, a card terminal, an e-invoice
provider, an ERP — each is a system we integrate *with*, not a system the core should *know
about*. Roadmap v3 makes "plug-and-play with any vendor, in any country" a first-class principle,
and a principle with no enforcement decays: six months of pressure to ship a partner turns into a
`if vendor == "grab"` in the billing path, and the core is no longer a core.

The dependency rule (ADR-0013) already forbids `pos-core`/`pos-ports`/`pos-proto` from *depending*
on infrastructure crates, which structurally keeps adapters out of the core's link graph. That is
necessary but not sufficient: a vendor name can leak into core *logic or data* — an enum variant,
a match arm, a string literal compared against — without adding a single dependency edge. This ADR
names the boundary and adds the tripwire.

**Decision.**

1. **The core holds only the POS invariant.** `pos-core`, `pos-ports` and `pos-proto` contain the
   order/bill/shift/table machines, the billing arithmetic, the permission and capability model,
   the config-tree shapes, and the event catalogue — the things true of *any* point of sale. They
   do not contain the name, branded behaviour, wire format, or special case of any particular
   external system or vendor.

2. **Every external system plugs in through exactly one of three points:**

   - **A port + adapter** — for a side effect or a device: `Printer`, `PaymentTerminal`,
     `Fiscalization`, `ShippingDispatch`, `DeliveryVendor`, `ErpSink`. The port is a trait in
     `pos-ports` with a contract-test suite and a fake; the adapter is a separate crate. Swapping
     the adapter never touches the core. A vendor's identity lives in the adapter and, where it must
     reach the domain, as **opaque data** (an `Open<T>` wire enum, a free-text `DisplayName`, an
     id) — never as a branch in core logic.

   - **An API contract + a scoped key** — for data in and out: the `/v1` surface, authenticated by a
     tenant-scoped API key. An integrator sending or reading data speaks this contract; the core
     does not learn who they are.

   - **An event stream** — for realtime: `/ws` at the store (a scoped, filterable, resumable
     subscription — the same one the built-in KDS consumes, dogfooded) and cloud webhooks for
     internet-side SaaS. The core emits its catalogue; subscribers select what they need.

3. **A vendor whose intake format differs from the contract gets a connector, not a core change.**
   A thin per-vendor connector (on the cloud, built from `templates/adapter-template`) receives the
   vendor's own webhook, verifies the vendor's own signature, and transforms it into the internal,
   idempotent `/v1/orders`. Adding a marketplace is writing one connector's data mapping.

4. **A CI gate keeps vendor names out of the core.** A new `xtask` check —
   `vendor-neutral-core` — scans the production source of `pos-core` and `pos-proto` (comments and
   `#[cfg(test)]` modules excluded, since example data and doc prose legitimately name vendors) for
   a denylist of vendor/marketplace/acquirer/e-invoice brand names, and fails the build if one
   appears. The denylist is a curated tripwire, not a proof; it is the automated half of a rule the
   reviewer holds. It is deliberately narrow — unambiguous brand tokens only, never domain words
   that a vendor also happens to use (e.g. `pax` as covers, `upi` as a payment rail).

**Deliberately not done.**

- **Not a plugin runtime.** Adapters are compiled Rust crates chosen at composition, not
  dynamically loaded modules. Plug-and-play here means *a stable seam*, not a marketplace of
  binaries running untrusted code inside the edge.
- **Not applied to `pos-ports`' own port names.** A port trait may name the *category* it abstracts
  (`PaymentTerminal`, `DeliveryVendor`); the gate scans `pos-core` and `pos-proto`, the layers that
  must be free even of categories' brand instances. `pos-ports` is governed by the dependency rule
  and review.

**Consequences.**

- The plug-and-play principle becomes enforceable rather than aspirational: a PR that couples the
  core to a vendor goes red in CI with the offending file and line annotated, the same way the
  dependency rule (ADR-0013) turned layering into law.
- Every external integration has one obvious home — a port, the API, or the event stream — so
  "how do we integrate X?" has a decision tree instead of an argument. `docs/roadmap-v3.md`'s
  Wave W9 builds the connector framework and the proof kit on this foundation.
- The gate is additive and vendor-neutral itself: extending the denylist is a one-line change with
  no architecture review, and a false positive (a domain word that collides with a brand) is fixed
  by narrowing the token, never by weakening the boundary.
- No wire, permission, migration, or dependency change; the core is already vendor-neutral today
  (the two brand-name occurrences in `pos-proto` are example data inside a test and a doc comment),
  so this ADR ratifies the existing state and guards it.
