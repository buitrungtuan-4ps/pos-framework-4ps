# `countries/zz` — the reference country module

`ZZ` is CLDR's code for an unknown region, so it can never collide with a real country. This
directory exists to be **copied**.

## Starting a real country

1. `cp -r countries/zz countries/<cc>` — lower-case ISO 3166-1 alpha-2.
2. Rename the package to `pos-country-<cc>` in `Cargo.toml`, and the type in `src/lib.rs`.
3. Add `"countries/<cc>"` to the workspace `members` in the root `Cargo.toml`.
4. Add a feature to each binary that should carry it:

   ```toml
   [features]
   country-<cc> = ["pos-country-<cc>"]
   ```

5. Add an arm to that binary's `country_registry!` invocation, guarded by the feature.
6. Fill in `locale_pack` with what the country's law actually says, and replace `Fiscalization`
   with the real provider.

Step 3 without step 4 fails CI, so the edit that is easiest to forget is the one that is checked.

## What belongs here, and what does not

The line is [ADR-0027](../../docs/adr/0027-country-modules.md): **this module ships what the law
says; the configuration tree overrides it.**

| Here | In the configuration tree |
|---|---|
| Currency | Which `tax_class` each menu item carries |
| Default channel-keyed tax rate table | An override of that table, per store |
| Invoice number format and legal lifecycle | Whether tips and service charge are enabled |
| Tax-code *format* validation | The store's timezone |
| Default retention period | The retention period actually chosen |

Two things are deliberately **not** here.

**The store's timezone.** Indonesia spans three and the United States spans six, so a
country-level timezone would be wrong in the countries where it mattered most.

**Whether a tax code is registered.** That is a call to the tax authority and belongs behind
`Fiscalization`. Keeping it out of `is_valid_tax_code` is what lets a store check a corporate
customer's tax code with no internet.

## What this module does not implement

`Fiscalization` here allocates numbers from a local counter and never contacts an authority,
because there is no `ZZ` tax authority to contact. It is a **working** implementation rather than a
stub — it passes the full `Fiscalization` contract suite, including offline issuance and
never-reuse — so the shape a real country fills in is already proven. See `tests/contract.rs`.

A real module replaces the body and keeps the suite.
