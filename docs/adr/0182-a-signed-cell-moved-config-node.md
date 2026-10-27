# ADR-0182 — A signed "cell moved" config node

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-10-27 · Implements item C9
of [ADR-0179](0179-cells-the-directory-and-the-front-door.md) · Relates to
[ADR-0033](0033-config-tree.md), [ADR-0039](0039-config-delivery.md),
[ADR-0047](0047-minisign-verification.md)

Waits on the owner's approval. It adds a node type to `pos-proto`, which needs an owner review, and
no code changes until it is Accepted.

## Context

[ADR-0179](0179-cells-the-directory-and-the-front-door.md) says a device resolves its cell once and
then stores its `cloud_url`, and that "if its tenant moves, a signed 'cell moved' notice reaches it
through the config pull". Stores only make outbound connections and the cloud never pushes
([ADR-0039](0039-config-delivery.md)), so the notice must travel the way all configuration does: as a
node the store pulls from the cell it already talks to, its **old** cell.

That makes the node unlike every other one. Every other node only changes how a store trades. This
one changes **where the store sends its events and fetches its configuration**. A store that obeyed
a forged notice would ship its event log to whoever forged it. The config pull is authenticated by the
store's scoped key and travels over TLS, but both only prove the store reached *a* cloud holding *a*
key. They do not prove the owner decided to move the tenant.

## Options considered

**A. An ordinary config node, trusted because it came over the config pull.** Simple. A compromised
or misconfigured cell could then redirect a whole fleet's events with one publish.

**B. A node carrying the move, signed with the directory key.** The store checks the signature
against the directory public key it already trusts (ADR-0179 C2) before it acts. The cell relays the
notice but cannot write one.

**C. No notice: the store re-resolves through `/.well-known/pos-cell` on every boot.** Every boot
then depends on the old cell answering, and a store that never reboots never moves.

## Decision

**Option B.**

- **The node.** `cell_moved`, a new config node type in `pos-proto`, carrying `tenant_slug`,
  `from_cell_id`, `to_cell_id`, `to_base_url`, `directory_version` (the directory that records the
  move), `cross_country` and `effective_time`, plus a minisign signature over the node's canonical JSON. It carries no
  personal data.
- **Who signs.** The directory key holder, never the release key and never a cell. The console of
  the old cell publishes a notice it was handed. It holds no key that could make one.
- **What the store does.** It verifies the signature against its directory public key. It checks
  that `tenant_slug` and `from_cell_id` are its own and that `directory_version` is newer than the
  directory it holds. Then it drains its outbox to the old cell, switches `cloud_url` to
  `to_base_url`, and fetches its configuration from there. A notice that fails any check is logged,
  reported as an alert, and ignored. The store keeps trading on its current cell either way.
- **Failover stays within a country** (ADR-0179), and a move across countries is never silent. A
  notice whose target cell's home country differs from the store's `locale` country must say so in a
  `cross_country` field, which the signature covers. The store records it and raises an alert, and
  is never refused on that ground alone (ADR-0179). A notice that crosses a country without saying
  so is ignored like any other notice that fails a check.
- **Redirects stay temporary until the move is final.** The front door answers `307` for the moved
  tenant while stores are moving, and `308` once the directory marks the move final.

## Consequences

- `pos-proto` gains one node type and `PROTOCOL_VERSION` is considered. The change is additive.
- The edge gains a verify-and-switch step behind the config apply. It is the first node the edge
  verifies with a key other than the release key, so the directory public key becomes edge
  configuration as well as cloud configuration.
- Moving a tenant needs the directory key holder twice: once for the directory, once for the notice.
  That is the price of a move a cell cannot forge.
- Until this lands, moving a tenant means re-activating its devices against the new cell.
