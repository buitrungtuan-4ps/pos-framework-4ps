# ADR-0183 — A country pack seeds a new store's settings, in its own currency

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-10-29 · Extends
[ADR-0105](0105-a-country-pack-is-values.md) and [ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md)
decision 1

Accepted by the owner on 2026-10-29.

## Context

A new store takes the owner's "new-store values" from the setting register: each `Setting` in
`crates/pos-proto/src/settings/nodes.rs` may carry one `preset`, which `POST /admin/settings/presets`
writes at the store's scope when the store does not set it already. That works for settings that
mean the same thing everywhere (`idle_lock_seconds = 120`). It does not work for a setting counted in
money. `shift.variance_reason_minor`, the over/short past which a drawer close asks for a reason, is
in the store's minor unit: 20,000 is a sensible threshold in đồng and a large one in yen. So it has
no preset, and every new store opens with the check off unless someone remembers to set it. The
pilot run found exactly that: a drawer closed 82,450 ₫ short in one tap.

The settings themselves are already flexible: any of them can be set per tenant, brand, store group
or store and published. What is missing is a correct **starting value** that depends on the store's
country, and the rule from ADR-0105 is that a country's facts are values in its pack, never code in
the framework.

## Options considered

1. **One global preset** (`Some(20_000)` in the register). Rejected: right for Vietnam, wrong for
   every other currency, and a currency fact hard-coded in `pos-proto`.
2. **A preset per currency inside the register.** Rejected: the register would learn every
   country's coinage, which is what country packs are for.
3. **Leave it to each operator.** Works (the tenant scope covers every store), but a new tenant still
   starts unsafe, and the runbook becomes the only guard.
4. **The country pack seeds settings** (chosen).

## Decision

1. `CountryModule` gains `setting_defaults()`, a list of *(setting key, value)* pairs in that
   country's units. It is values only (ADR-0105): no logic, no clock. The default implementation
   returns an empty list, so a pack that says nothing changes nothing.
2. A setting key a pack names must exist in the register, and its value must pass that setting's
   shape (bounds, enum members). A test over every compiled-in pack enforces both, so a renamed or
   tightened setting fails the build rather than a store.
3. Applying new-store values resolves each setting as: **the store's country pack** → the register's
   global `preset` → nothing. As today, only a setting the store does not already set is written,
   at the store's own scope, so it stays visible and editable in the console.
4. Choosing a store's country in Store settings offers the pack's setting defaults the same way it
   fills the locale fields ([#1036](https://github.com/buitrungtuan-4ps/pos-framework-4ps/pull/1036)):
   unset values are filled, set values are kept, and the console says which.
5. Nothing here limits configuration: any value can still be set and overridden per tenant, brand,
   store group or store. The pack only removes the unsafe empty start.
6. First use: `countries/vn` seeds `shift.variance_reason_minor = 20_000` (đồng), confirmed by the
   owner on 2026-10-29. Other packs add their own when an owner confirms a figure; none is guessed.

## Consequences accepted

- `pos-country`'s trait grows one method with a default, so no existing pack or caller breaks. No
  wire, event or `PROTOCOL_VERSION` change; the presets route's answer lists the settings it wrote,
  as it already does.
- A threshold is now a policy value in a country directory, reviewed in that pack's pull request, and
  not a constant anywhere in the framework.
- A tenant operating in one country across several currencies is unusual and keeps the escape it
  has today: set the value per store or store group.
