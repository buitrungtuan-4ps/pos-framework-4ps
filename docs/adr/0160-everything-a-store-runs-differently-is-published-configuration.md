# ADR-0160 — Everything a store runs differently is published configuration: typed, defaulted, authored once for many stores, and honoured or hidden

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-26
· Relates to [ADR-0004](0004-cloud-owned-configuration.md), [ADR-0033](0033-config-tree.md),
[ADR-0071](0071-config-without-json.md), [ADR-0078](0078-sync-and-ota-closure.md),
[ADR-0105](0105-a-country-pack-is-values.md), [ADR-0115](0115-reason-codes-are-a-managed-list.md),
[ADR-0122](0122-a-store-group-is-a-delivery-cohort.md), [ADR-0158](0158-the-till-enforces-each-persons-own-permissions.md)

## The problem

The owner decided on 2026-09-26 that configuration comes from the cloud and can differ per store as
the situation needs, that print language is a setting, and — as a rule — that nothing a store might
run differently is hardcoded. [ADR-0004](0004-cloud-owned-configuration.md) and
[ADR-0033](0033-config-tree.md) already say configuration lives in the cloud, but in practice:

- **One level.** Each store has its own tree with its own copies of the four layers; the console
  writes only the Store layer; the Tenant and Brand layers can be set only as raw JSON, one store at a
  time; the Device layer is one document per store, not per till.
- **Hardcoded values stores differ on:** tip presets (5/10/15 %), split ways (2–6), receipt width,
  code page, cut and drawer kick, the KDS late threshold (600 s), the drawer model (one shared drawer
  per store, though `docs/pos-spec.md` §6 says per cashier device), selling with no open shift
  (allowed), the opening float (none), receipt labels that follow the display language only in
  Vietnamese (every other language prints English, #526), one language for receipts and kitchen
  tickets, auto-print on settle, tax rounding (half-up), and which channel a walk-in or counter order
  takes.
- **Values in the store's local file**, against ADR-0004: the sign-in idle timeout, the print font
  size and the backup interval.
- **Switches that do nothing.** `pay_first_enabled`, `queue_number_enabled`, `tabs_enabled`,
  `barcode_enabled` and `qr_ordering_enabled` have no reader at the edge; QR ordering is governed by
  three overlapping switches; campaigns, inventory auto-86 and vendor policies are delivered and never
  applied; `kds_enabled` only hides screens.
- **Published but ignored:** the edge installs only the dine-in price book, so takeaway, delivery and
  QR orders are priced at dine-in prices while taxed at their own channel's rate.
- **Open tills learn of a change late** — only at sign-in or reload.

## Options considered

| | Option | Why not / cost |
|---|---|---|
| A | Keep the constants; change them by release | The owner's rule forbids it; every store would share one value |
| B | A free-form settings bag of keys and values | Untyped, unvalidated, and the JSON editing [ADR-0071](0071-config-without-json.md) removed |
| C | **Typed fields with defaults, authored at any scope, honoured by the edge or hidden by the console** | Many small slices across the edge, the till and the console |

## Decision

Option **C**. The owner approved it on 2026-10-01.

1. **A setting is a typed field with a default.** Every value a store may run differently is a field
   on a published node, typed in `pos-proto` as the `integrations` node is
   ([ADR-0153](0153-a-vendor-is-a-provider-the-cloud-chooses.md)), validated by the cloud, with a framework default the
   binary carries and, where the country matters, a country default from its pack
   ([ADR-0105](0105-a-country-pack-is-values.md)). An absent field means its default, never "off" by
   accident, as reason codes already work ([ADR-0115](0115-reason-codes-are-a-managed-list.md)).
   **The default is today's behaviour**, so an upgrade changes nothing until someone sets a value; new
   stores take the recommended values from the console's store presets. The exceptions are today's
   defects, which are corrected rather than preserved: receipt labels print in the receipt's language
   in every language with a translation, not only Vietnamese (item 2), and each channel prices at its
   own prices (item 8).
2. **The first batch** is what was found hardcoded:
   - `tender`: tip presets, split ways, quick-cash keys per currency;
   - a new `shift` node: drawer model (`PER_STORE`, `PER_TERMINAL`, `PER_CASHIER`), selling with no
     open shift (`ALLOW` or `REFUSE`), a default opening float, and a blind close. Who sees takings on
     the Today screen is a permission, `reports.takings.view`, in
     [ADR-0158](0158-the-till-enforces-each-persons-own-permissions.md)'s catalogue;
   - a new `printing` node: the receipt's languages (one, or two for a bilingual receipt), whether the
     receipt prints on settle, and the labels in the receipt's language — #526 took them from the
     display language, for Vietnamese; the setting makes the print language a choice of its own; each station's
     kitchen-ticket language on the `stations` node; each printer's width, code page, cut and drawer
     kick on the `devices` node;
   - a new `session` node: the till's idle lock, the sign-in idle timeout, the PIN length and the
     lockout (attempts and minutes);
   - the KDS late threshold per station; the tax rounding mode on `locale`; the walk-in and counter
     channels on `channels`;
   - on `qr`, whether a guest's QR order joins the table's order or stays a separate order on the same
     bill — a question the owner has not answered, so it becomes a setting, defaulting to join.
3. **Author once, apply to many.** The console writes a setting at tenant, brand, store-group or
   store scope. The cloud resolves each store's value — most specific scope wins, the country pack
   beneath the tenant — and publishes one flat document, compiling at write time as store groups
   already do ([ADR-0122](0122-a-store-group-is-a-delivery-cohort.md)). This is the shared layer
   ADR-0033 deferred, built without a fifth merge level at the edge.
4. **A till is a device, and a device can differ.** The Device layer becomes one entry per paired
   device, so a bar till can have its own drawer, printer or receipt language. A person's own display
   preferences — the till's UI language, the kitchen chime — stay on the device; anything that changes
   what the store does or prints is configuration.
5. **Honour or hide.** Every switch the console offers changes behaviour at the edge. A switch the
   store's release does not honour yet is hidden for that store — the cloud knows each store's release
   ([ADR-0078](0078-sync-and-ota-closure.md)) — until it does. QR ordering gets one switch,
   `qr_ordering_enabled`, from which `qr.enabled` and the QR channel follow; a migration sets it from
   what each store does today, so no store's QR ordering changes on upgrade. A drawer model other than
   one per store is offered only once the multi-drawer shift work that needs it has landed.
6. **Nothing business-facing lives in the local file.** The idle timeout, the font size and the
   backup interval move into configuration; the file keeps only what the box needs before it can reach
   the cloud — the cloud address and local paths.
7. **Open tills refresh.** When the edge applies a new version it tells connected tills over `/ws`,
   and they reload menu, locale, floor, layout and settings without a new sign-in.
8. **Every channel prices at its own prices.** The edge installs each channel's catalogue from the
   menu book and prices an order at the order's channel. This corrects a defect rather than adding a
   setting, so it lands ahead of this record, as the owner asked on 2026-09-30.
9. **One register of settings.** `docs/configuration.md` lists every setting — node, field, default,
   scope, release — generated from the `pos-proto` types and diffed in CI like the other snapshots, so
   a setting cannot be removed or renamed silently (AGENTS.md §2).

## Before this is accepted

- The owner confirmed on 2026-09-30 that a new store refuses selling with no open shift, while an
  existing store keeps allowing it until someone changes the setting.
- The owner confirms the other recommended values for new stores: the drawer model, the receipt
  languages and the idle lock.
- The owner confirms one QR switch (item 5) and joining the table's order by default (item 2).

Met on 2026-10-01: the owner confirmed the recommended values for a new store: one drawer per
terminal once the multi-drawer shift work lands, and one per store until then; receipts in the
country's language (Vietnamese in Vietnam), with a bilingual Vietnamese and English receipt as a
per-store choice; and a till that locks after two minutes without a touch. The owner also
confirmed one QR switch, and that a QR order joins the table's order by default.

## Consequences accepted

- **`pos-proto` grows typed nodes** where the edge keeps private structs today (`locale`, `qr`,
  `retention`), and needs the owner's review.
- **Many small slices.** Each moves one group of values, adds its console form, and updates the
  register; none changes behaviour for a store that sets nothing.
- **The rule has a limit.** A setting exists because a store runs the value differently, not because
  one might (`docs/design-principles.md`, YAGNI). The register makes each one visible, so a setting
  nobody sets can be questioned.
- **A mistake is one publish away.** Validation, the effective-value preview, history and rollback
  (ADR-0033) are what make that acceptable.
