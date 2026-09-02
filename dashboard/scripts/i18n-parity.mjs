// The locale-parity gate (ADR-0020, roadmap v3 Q2).
//
// Every locale catalogue must carry exactly the key set `en.json` carries. `en` is the enforced
// floor: `MessageKey = keyof typeof en` in `src/i18n/index.ts`, and `t()` falls back to the English
// message for any key a translation is missing.
//
// # What this checks that nothing else does
//
// Precisely one property, and it is worth being exact about the scope so nobody over-trusts this
// script. A *bad* key is already impossible: `t()` takes `MessageKey`, so a typo'd or absent key is
// a type error and `tsc --noEmit` — which runs ahead of this in `pnpm build` — rejects it.
//
// What the type system cannot see is the other catalogues. A key added to `en.json` and forgotten in
// `vi.json` type-checks perfectly and *works*: the runtime falls back to English. So it ships, and a
// Vietnamese operator gets an English string in the middle of a shift, with nothing failing anywhere
// to say so. The fallback is the right runtime behaviour — a blank or a raw key on a till would be
// worse — which is exactly why it needs a build-time check instead: the safety net is silent.
//
// The reverse direction matters too, if less urgently: a key in `vi.json` that `en.json` has dropped
// is dead weight and usually the leftover half of a rename.
//
// Run by `pnpm i18n:parity`, which `pnpm build` invokes.

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const I18N = fileURLToPath(new URL("../src/i18n", import.meta.url));
const FLOOR = "en";

function catalogue(locale) {
  const parsed = JSON.parse(readFileSync(join(I18N, `${locale}.json`), "utf8"));
  // The catalogues are flat: keys are dotted strings, not nested objects, because `MessageKey` is
  // `keyof typeof en` and a nested shape would type only the top level. Guard the assumption rather
  // than silently comparing the wrong thing if someone nests one later.
  const nested = Object.entries(parsed)
    .filter(([, value]) => typeof value !== "string")
    .map(([key]) => key);
  if (nested.length > 0) {
    console.error(
      `i18n-parity: ${locale}.json has non-string values (${nested.slice(0, 5).join(", ")}).\n` +
        "  The catalogues are flat by design — dotted keys, string values — because MessageKey is\n" +
        "  `keyof typeof en`. Flatten it, or this gate and the key type are both comparing the wrong\n" +
        "  thing.",
    );
    process.exit(1);
  }
  return new Set(Object.keys(parsed));
}

const locales = readdirSync(I18N)
  .filter((name) => name.endsWith(".json"))
  .map((name) => name.slice(0, -".json".length))
  .sort();

if (!locales.includes(FLOOR)) {
  console.error(`i18n-parity: no ${FLOOR}.json, and ${FLOOR} is the floor every locale is compared to.`);
  process.exit(1);
}

const floor = catalogue(FLOOR);
let failed = false;

for (const locale of locales) {
  if (locale === FLOOR) continue;
  const keys = catalogue(locale);
  const missing = [...floor].filter((key) => !keys.has(key)).sort();
  const extra = [...keys].filter((key) => !floor.has(key)).sort();

  if (missing.length > 0) {
    failed = true;
    console.error(
      `i18n-parity: ${locale}.json is missing ${missing.length} key(s) that ${FLOOR}.json has.\n` +
        "  These render in English at runtime rather than failing, so nothing else catches them:",
    );
    for (const key of missing.slice(0, 20)) console.error(`    - ${key}`);
    if (missing.length > 20) console.error(`    … and ${missing.length - 20} more`);
  }

  if (extra.length > 0) {
    failed = true;
    console.error(
      `i18n-parity: ${locale}.json has ${extra.length} key(s) ${FLOOR}.json does not.\n` +
        `  Unreachable — ${FLOOR} is the key list — so this is dead weight or half a rename:`,
    );
    for (const key of extra.slice(0, 20)) console.error(`    - ${key}`);
    if (extra.length > 20) console.error(`    … and ${extra.length - 20} more`);
  }
}

if (failed) process.exit(1);

const others = locales.filter((locale) => locale !== FLOOR);
console.log(
  `i18n-parity: ok — ${floor.size} keys, and ${others.join(", ") || "no other locale"} ` +
    `match ${FLOOR} exactly.`,
);
