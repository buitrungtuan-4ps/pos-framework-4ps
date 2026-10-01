# Configuration register

Generated from `crates/pos-proto/src/settings.rs`. Do not edit by hand — run `just snapshot`.

Every value a store may run differently is a setting: a typed field on a node of the store's published configuration ([ADR-0160](adr/0160-everything-a-store-runs-differently-is-published-configuration.md)). A store whose configuration does not set a value runs the default, which is what the edge did before the setting existed, so an upgrade changes nothing until someone sets one. A value the edge does not recognise also reads as the default.

- **Where it is set** lists the scopes a value may be written at, in the console's settings or with `PUT /admin/settings`. A store runs the value of the most specific scope that sets one: the store, then its store groups (the value written last, if two disagree), then its brand, then the tenant (ADR-0160 decision 3).
- **New store** is the value the console gives a store it creates, where that differs from the default.
- **Honoured from** is the first release that honours the setting. A store running an earlier release ignores it.

| Setting | Values | Default | New store | Where it is set | Honoured from | What it decides |
|---|---|---|---|---|---|---|
| `shift.no_shift_selling` | `NO_SHIFT_SELLING_ALLOW`, `NO_SHIFT_SELLING_REFUSE` | `NO_SHIFT_SELLING_ALLOW` | `NO_SHIFT_SELLING_REFUSE` | tenant, brand, store group, store | 0.14.1 | Whether a till may seat a table, start a counter order or take a payment while no shift is open. `NO_SHIFT_SELLING_REFUSE` refuses each of them with `OPEN_SHIFT_REQUIRED` until a shift opens. |
