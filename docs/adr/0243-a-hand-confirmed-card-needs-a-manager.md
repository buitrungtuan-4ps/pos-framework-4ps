# ADR-0243 — A card confirmed by hand needs a manager, by default

**Status** Accepted 2026-11-25 under the owner's standing condition that business preferences are
published settings and money-integrity rules default to the safe choice · **Owner**
@maintainers-architecture · **Date** 2026-11-25 · Phase 10 (roadmap-v4 row 10) · **Amends decision 3
of [ADR-0230](0230-a-card-is-taken-through-the-terminal-port-and-an-unknown-result-parks-the-bill.md)**
(the `CONFIRM` exit) · Relates to
[ADR-0158](0158-the-till-enforces-each-persons-own-permissions.md),
[ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md),
[ADR-0183](0183-a-country-pack-seeds-a-new-stores-settings.md)

## Context

ADR-0230 decision 3 lets anyone who may take payment resolve a parked card with `CONFIRM`: staff read
the terminal's slip and record the card as captured. The phase 10 security review (finding **C1**,
Medium) found that this turns an unknown card into a paid bill on one person's word, with nothing
from the terminal. A cashier who took cash could settle the bill this way and keep the cash.

## Options considered

1. **Keep it** and rely on a daily check against the terminal's batch.
2. **Forbid `CONFIRM`**: a store whose terminal answers nothing could not close the bill.
3. **A second person, by default**, as a void needs, with the store able to turn it off.

## Decision

**Option 3.**

1. **A published setting** (ADR-0160) on the `billing` node, `billing.card_confirm_approval`, at every scope:
   `CARD_CONFIRM_APPROVAL_MANAGER`, the register's default, and `CARD_CONFIRM_APPROVAL_NONE`. No country pack and no
   template changes it, so every store starts with `MANAGER`.
2. **Under `MANAGER`, `CONFIRM` needs a manager**: the step-up a void of an open bill uses, checked
   against `billing.bill.void` (a manager's badge and PIN, unless the person confirming holds it
   directly). Without one it is refused like a void without one. `LOOK_UP` and `CANCEL` are unchanged:
   each asks the terminal.
3. **Audited with both people.** The capture a `CONFIRM` records names the person who confirmed and
   the approver, as every stepped-up act does.
4. **The drawer's close counts them.** The close card shows how many card payments were confirmed by
   hand in the shift, and their total.

## Consequences accepted

- A store with no manager on the floor cannot close a parked card by hand until one comes, or it sets
  `CARD_CONFIRM_APPROVAL_NONE`.
- **Open owner decisions:** the owner may set `CARD_CONFIRM_APPROVAL_NONE` per store. A daily check of hand-confirmed cards
  against the terminal's batch is recommended either way.
