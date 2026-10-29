---
kind: added
---
- **A register of supported UI languages, and a check that each is complete** (P11 item 2,
  ADR-0020). `packages/shared/i18n/languages.json` lists `en` and `vi`. `cargo xtask i18n`, now in
  `just preflight`, fails when the till or the console lacks a catalogue for a supported language,
  ships one for a language the register does not list, has a `LOCALES` that differs from it, or has
  a catalogue missing or adding keys against `en.json`. In the console it also fails when a supported
  catalogue lacks a setting's title, help, value labels or confirmation notice. A language a country
  module declares but the register does not list (`ja`, from `jp`, today) is reported as declared,
  not yet supported. `docs/languages.md` describes the register, how a language is added, and the
  fallback to English, which `dashboard/tests/i18n-fallback.test.ts` now tests. No translation is
  added.
