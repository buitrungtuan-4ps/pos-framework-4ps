---
kind: added
---
- **`cargo xtask new country <cc>` writes a country module from the values its law sets**
  (ADR-0176). Each value is a required flag, and the command refuses to run without one:
  - the currency (ISO 4217, read with `CurrencyCode::parse`) and its exponent;
  - whether prices include tax;
  - the cash rounding;
  - a tax rate for food, alcohol and exempt items, with optional per-channel overrides;
  - the default retention;
  - the language.

  The module's fiscal half is the shared offline allocator, so it passes the `Fiscalization`
  contract suite from its first build. The command adds a `country-<cc>` feature, an optional
  dependency and a registry arm to `pos-edge` and to `pos-cloud`, and makes the country part of
  no default feature.
