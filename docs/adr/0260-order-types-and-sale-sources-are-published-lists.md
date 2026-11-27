# ADR-0260 — Order types and sale sources are published lists

**Status** Accepted 2026-11-27 under the owner's request; the points marked "to confirm" wait for the
owner · **Owner** @maintainers-architecture · **Date** 2026-11-27 · Phase 11 · Supersedes in part
[ADR-0259](0259-a-sale-is-reported-by-its-channel-and-its-order-type.md) decisions 2 and 3 · Details
[ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md)

## Context

The owner, 2026-11-27: "Các kênh (channel) hay type có thể tùy chỉnh flexible add thêm hoặc bổ sung".
HQ must be able to add order types ("Tự đến lấy", drive-through, catering) and sale sources (kiosk,
hotline, corporate, website) without a release. ADR-0259 fixed three order types in code and
reserved `SALE_SOURCE_KIOSK`. Money, tax, price lists, service charge and fiscal rules key off
`SalesChannel`, and must keep doing so.

## Options considered

1. **More `SalesChannel` / `OrderType` tokens in code.** Every new type is a release. Refused.
2. **A type is a price/tax variant of its own.** Lets configuration change money rules. Refused.
3. **Published lists of tokens, each type tied to a fixed base kind; the order records its tokens**
   (chosen). Reports key by token, so a new entry needs no code.

## Decision

1. **A new node, `order_channels`**, two ADR-0160 settings, scopes tenant, brand, store group and
   store; the most specific scope's list wins whole. A new node, not `channels`: an older edge reads
   a `channels` node without `enabled` as accepting no channel.
   - `order_types` (kind `SETTING_KIND_ORDER_TYPES`, at most 32): `order_type` token, `base_order_type`
     (`ORDER_TYPE_DINE_IN`, `_TAKEAWAY` or `_DELIVERY`; a built-in's base is itself), `names`
     (language → name), `active` (default true), `default` (at most one: what the till preselects),
     `applies_to_sources` (at most 3 of `SALE_SOURCE_QR`, `_API`, `_APP`). Default value: the three
     built-ins, which is ADR-0259; an empty list reads the same.
   - `sources` (kind `SETTING_KIND_SALE_SOURCES`, at most 64): `source` token, `names`, `active`.
     Default: empty, the built-ins only. Listing a built-in only renames it.
2. **Tokens and names.** A token is `ORDER_TYPE_` or `SALE_SOURCE_` then upper-case letters, digits
   and `_`, at most 48 characters, never reused for another meaning. A custom entry needs an `en`
   name; names are at most 48 characters, at most 4 languages. A name never enters an event.
3. **Never deleted.** The cloud refuses a write that drops a token the same scope's previous value
   held; HQ sets `active: false`. A built-in type may be deactivated (the till stops offering it).
4. **`sales.order.opened` gains two optional fields**, `order_type` and `sale_source`, each a token
   as above, left off the wire while absent. A token cannot carry a name, phone or e-mail.
   - The till (`POST /api/orders`) may name an active `order_type` and an active custom
     `sale_source` (or `SALE_SOURCE_POS`). The type's base kind sets the order's `SalesChannel`; a
     `channel` that disagrees is refused. The till's New order offers the active types, `default`
     first, where the store asks walk-ins (`WALK_IN_CHANNEL_ASK`) or lists an active custom type, and
     the active sources where it lists a custom one; otherwise it opens as today.
   - A kiosk's order records `SALE_SOURCE_KIOSK`: the reserved token becomes real, replacing
     ADR-0259's planned `opened_on_kiosk` flag.
   - A QR, API or app order records the first active type whose `applies_to_sources` names its
     source and whose base kind is the one ADR-0259 derives for it; else nothing is recorded.
5. **Reading.** `pos_proto::sale_channel` takes the two fields too: a recorded token wins; absent
   (every older order) reads exactly as ADR-0259 derives it. An app's order stays `SALE_SOURCE_APP`
   with its connection.
6. **Reports key by token and show the store's current names**: the till's X and the console's X/Z
   gain `order_type_names` and `source_names` (token → names), read as the report is served, as
   `vendor_names` is. A token no list names shows its built-in label, else the token. The printed X
   prints the channel · type lines.
7. **Fixed, not configurable**: price, tax, service charge, fiscal and numbering follow the base kind
   through `SalesChannel`; the edge checks the list, the base kind and the limits.

## Consequences accepted

- `SaleChannel` carries tokens rather than closed enums; `SaleSource` and `OrderType` stay as the
  built-in tokens. Report wire shapes are unchanged: they always carried token strings.
- **To confirm by the owner:** a per-connection type for one app only (today `applies_to_sources`
  covers every app); whether reports also group by base kind.
