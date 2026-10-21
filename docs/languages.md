# UI languages

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-10-21

Which languages the till (`ui/`) and the console (`dashboard/`) speak, how a language is added, and
what a person sees when a message is missing. The runtime is
[ADR-0020](adr/0020-i18n-runtime.md): ICU MessageFormat, with `en` the floor.

## The register

[`packages/shared/i18n/languages.json`](../packages/shared/i18n/languages.json) lists the supported
languages: today `en` and `vi`. A language is listed only when **both** front ends ship a complete
catalogue for it, `src/i18n/<language>.json`, reviewed by a native speaker. A machine translation is
never shipped as a product string: it can be a draft for that reviewer, kept outside the repository.

`cargo xtask i18n`, in `just preflight`, holds the register to the code. For each front end it fails
when a supported language has no catalogue, when a catalogue ships for a language the register does
not list, when `LOCALES` in `src/i18n/index.ts` is not the register, and when a catalogue lacks a key
`en.json` has or carries one it does not. In the console it also fails when a supported catalogue
lacks a setting's words: the title and help of every key in
[`snapshots/settings.txt`](snapshots/settings.txt), the label of every token value, and the notice of
every value that needs a recorded confirmation. Without them the Settings screen shows the raw key.

## Languages a country declares

Each country module names its default language (`default_language` in `countries/<cc>/src/lib.rs`):

| Country module | Declares | In the register |
|---|---|---|
| `vn` | `vi` | yes |
| `jp` | `ja` | not yet |
| `in` | `en` (an Indian tax invoice is written in English; a store serving guests in another language publishes it as its display language) | yes |
| `zz` (the template) | `en` | yes |

`cargo xtask i18n` names each declared language the register does not list as *declared by a
country, not yet supported*. That is a report, not a failure: the language enters the register only
with its complete, reviewed catalogues.

## When a message is missing

- **A key a catalogue lacks** falls back to the English message for that key, never to a blank or the
  raw key (`messageIn` in each front end's `src/i18n/index.ts`; `dashboard/tests/i18n-fallback.test.ts`).
  A supported catalogue lacks none, which `cargo xtask i18n` and each build's `i18n:parity` hold.
- **A language the front end has no catalogue for**, such as a store whose display language is `ja`
  today, leaves the device in the language it already shows: English, unless its operator chose
  another (`adoptStoreLanguage` in `ui/src/i18n/index.ts`).
- **A key the server names at run time** that this front end does not ship yet (`tFromServer`) shows
  its fallback, usually the key or token itself.

## Adding a language

1. Both catalogues, `ui/src/i18n/<language>.json` and `dashboard/src/i18n/<language>.json`, with every
   key `en.json` has, the settings' words among them, written or reviewed by a native speaker.
2. The language in each front end's `index.ts`: its import, `Locale`, `LOCALES`, `CATALOGUES` and its
   endonym.
3. The language in the register.
4. A `changes/` file.
