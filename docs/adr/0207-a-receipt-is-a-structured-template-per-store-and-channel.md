# ADR-0207 — A receipt is a structured template, per store and per sales channel

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-11-04
· Records roadmap-v3 D10 and B3.2 · Relates to
[ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md),
[ADR-0194](0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md) decision 7,
[ADR-0100](0100-receipt-and-ticket-printing.md), [ADR-0129](0129-a-receipt-itemises-what-was-sold.md),
[ADR-0164](0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md),
[ADR-0043](0043-translation-grid.md), [ADR-0075](0075-media-and-file-rail.md)

Proposed: it waits on the owner's acceptance, because it adds a published config node to `pos-proto`
and decides which documents a personal value may print on.

## Context

Roadmap v4 phase 3 asks for "a receipt template per store and channel" (roadmap-v3 B3.2). Roadmap-v3
debate D10 settled that a receipt is a structured config node and not a template language. No record
gives that node a shape.

What a receipt prints today is fixed in code (`pos-edge-printing`): the store's header, the lines,
the totals and tax lines, the payments and, where one was captured, the buyer. The `printing` node
chooses only the language, a second language, whether a settle prints, the tax components and the
font size. ADR-0194 decision 7 lets a receipt or ticket print custom fields, and a personal one only
on the copy that belongs to the person. Nothing yet says which copy that is.

## Options considered

1. **Grow the `printing` node.** It is per store already, but a per-channel shape inside a node of
   scalar settings breaks the one-field-one-setting register of ADR-0160.
2. **A part of the package** (ADR-0189), with sales-channel variants. Per store would then mean a
   package pinned per store: a whole package version to change a footer.
3. **A `receipt` node of its own, authored as an ADR-0160 setting with a per-channel map** (chosen).

## Decision

1. **A new `receipt` node**, authored at tenant, brand, store group or store scope; the most specific
   scope wins, as for every ADR-0160 setting. A store with no `receipt` node prints exactly what it
   prints today.
2. **A base template and per-channel templates.** The node holds a `base` template and at most one
   template per `SalesChannel`. The order's channel picks its template, falling back to `base`. A
   channel token from a newer release falls back to `base`.
3. **A template is structure, not a language.**
   - `header_keys` and `footer_keys`: at most four translation keys each, from the tenant's
     translation grid (ADR-0043). They print in the receipt's language, falling back to `en`. A key
     without an `en` value is refused at write. No free text is ever stored in the node.
   - `sections`: on or off for the sections that may be left out (the item rows' modifiers, the
     payments, the change line, the second language). The sections a country's law requires (the
     seller, the receipt number, the lines, the totals, the tax lines and a captured buyer) cannot be
     turned off. Each country module states which they are, and the cloud refuses a template that
     turns one off.
   - `custom_field_ids`: the custom fields of the package (ADR-0194) that print, in order, under the
     header, each as its label and value. A field the running package does not declare is skipped,
     never an error. The till never refuses to print because of the template.
4. **Which documents a template governs.** The final receipt, its copy (ADR-0164), the pre-bill, and
   a custom record's slip. Kitchen tickets keep their station's own settings, and shift reports are
   not templated.
5. **A personal value prints only on the person's copy.** A template marks `person_copy` on at most
   the record slip and the buyer's copy of a tax invoice. A personal custom field listed for any other
   document prints its label and no value. A reprint prints a personal value only for someone holding
   `data.personal.read` (ADR-0206) who reprints the person's own copy.
6. **No logo yet.** Images do not reach the edge (ADR-0075 decision 3). A logo is a later, additive
   field, together with the decision that brings images to the store.
7. **Limits.** At most 64 custom fields per template and 16 per-channel templates. A node over 16 KiB
   is refused at write.

## Consequences accepted

- **An addition to `pos-proto`:** the `receipt` node and its types, additive and without
  `deny_unknown_fields`. The settings register and `docs/configuration.md` gain its fields, and the
  routes for writing it are additive.
- **Each country module states its mandatory sections.** This is values only, as ADR-0105 requires,
  in each country's own crate.
- **The template cannot reorder the legal body of a receipt.** That body stays in code, held by the
  golden files. The template decides what is said above and below it, and what may be left out.
- **Until ADR-0206 is accepted,** no personal field can be published (ADR-0206 decision 11), so
  decision 5 has nothing to print yet. It is decided now so that the template's shape does not
  change later.
