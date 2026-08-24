# ADR-0020 — The i18n runtime: ICU MessageFormat over the platform `Intl`, with `en` as the floor

**Status** Accepted · **Owner** @maintainers-domain · **Last reviewed** 2026-08-19
**Relates to** [ADR-0018](0018-http-websocket-stack.md) · [ADR-0005](0005-country-neutral-core.md) · [ADR-0007](0007-in-house-vs-dependency.md)

**Context.** The operator interface (P6) is one SolidJS app ([ADR-0018](0018-http-websocket-stack.md))
that must run in Vietnamese and English now and take more languages later without a rebuild of its
screens. Two of the specification's rules make the choice, not taste:

- **No hardcoded user-visible strings** (`AGENTS.md` §2, the seventh standing code rule), enforced —
  a string baked into a component is a language nobody can add and a phrase nobody can fix.
- **`en` is always present and is the fallback.** A missing translation shows English, never a blank
  or a raw key. The country-neutral core ([ADR-0005](0005-country-neutral-core.md)) already keeps
  language out of the domain; the UI is where a locale finally resolves.

The archive is specific about the hard part: plurals and layout width. A count needs the language's
plural rule (Vietnamese has one form, English two, and others up to six), and a layout must survive
text ~30% longer than English without breaking. Both are ICU MessageFormat problems.

**Decision.** Format messages with **ICU MessageFormat**, evaluated by
[`@formatjs/intl-messageformat`](0007-in-house-vs-dependency.md), over the plural, number and date
data the platform's `Intl` already carries. Concretely:

- **Messages are ICU strings in per-locale JSON catalogues** — `src/i18n/en.json`, `src/i18n/vi.json`
  — keyed by a dotted name (`floor.title`, `pay.change`). A message may use ICU arguments and the
  `plural`/`select` forms; `{count, plural, one {# item} other {# items}}` is resolved by the
  language's CLDR plural rule, not by an `if count === 1` that is wrong in most languages.
- **CLDR data comes from the engine, not from us.** `Intl.PluralRules`, `Intl.NumberFormat` and
  `Intl.DateTimeFormat` ship inside the embedded Chromium ([ADR-0018](0018-http-websocket-stack.md)),
  with the Unicode data already there. We bundle **no** CLDR tables — that would be tens of thousands
  of duplicated rules in a binary that is meant to stay small.
- **`en` is the floor, in code.** The active locale is a reactive signal; `t(key, args)` looks the key
  up in the active catalogue, falls back to `en`, and — only if `en` also lacks it, which the lint
  makes impossible in a merged build — returns the key itself so nothing renders blank. `en.json` is
  therefore the source of truth for *which keys exist*.
- **The rule is mechanically enforced.** A `pnpm i18n:lint` step parses every `.tsx` with the
  TypeScript compiler API and fails on any JSX text node containing a letter, and on a hardcoded
  string in a user-visible attribute (`placeholder`, `title`, `aria-label`, `alt`). After extraction,
  every visible string is a `{t(...)}` expression, so a bare word in the tree is a real violation. The
  `ui` CI job runs it, so a hardcoded string cannot merge.
- **Money and dates do not go through message arguments.** Money stays the integer-minor-unit
  formatter (ADR-0028's discipline, no float); dates use the store's timezone. i18n selects the
  *words*, not the *arithmetic*.

**Why `@formatjs/intl-messageformat` and not the alternatives.**

- **A hand-rolled MessageFormat parser** — rejected. Plural category selection and nested
  `select`/`plural` are exactly the kind of correctness surface [ADR-0007](0007-in-house-vs-dependency.md)
  says to take from a vetted implementation, like the CSPRNG and the timezone math. Getting Vietnamese
  vs. Arabic plural categories subtly wrong is a bug found by a native speaker in production.
- **A full framework (`i18next`, `LinguiJS`)** — rejected as more than the job needs: routing,
  backends, and pluralisation engines we would only partly use, against a rule (`intl-messageformat`
  is one small, focused package that leans on `Intl`).
- **Shipping our own CLDR data** — rejected: the engine already has it. Duplicating it is size for no
  gain.

**Consequences.**

- One dependency added to the UI (`@formatjs/intl-messageformat`), on the UI's own dependency surface;
  the Rust backbone is untouched.
- `en.json` is the canonical key list; a new screen adds its keys there first, then `vi.json`. A key
  present in `en` but missing in `vi` renders English — visible, not broken.
- The no-hardcoded-strings lint is now a merge gate, so the seventh standing rule is enforced for the
  UI exactly as the dependency-rule and naming lints enforce the others.
- Right-to-left scripts and per-locale number/date shapes are available through `Intl` when a
  right-to-left language is added; nothing in the screens hardcodes width or direction beyond the
  token system, which already budgets for longer text.
