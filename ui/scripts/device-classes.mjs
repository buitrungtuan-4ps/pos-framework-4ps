// The device-class gate: a screen names the hardware it adapts to, never a pixel width.
//
// `docs/ui-ux.md` §1 principle 9 has named four device classes since P6 — POS terminal, tablet,
// phone, kitchen display — and for just as long the screens adapted on Tailwind's stock `sm`, `lg`
// and `xl`. Those are names for widths. `lg:grid-cols-4` says the layout changes at 1024 pixels;
// `terminal:grid-cols-4` says it is the layout for a fixed till, which is a claim a reviewer can
// agree or disagree with. `styles/tokens.css` defines `tablet` and `terminal` beside Tailwind's
// stock names rather than instead of them, because that file is mirrored into `dashboard/` and the
// console has forty-five responsive rules written in the stock vocabulary — clearing the defaults
// would have flattened every one of them silently, which is the failure this gate exists to stop,
// committed in the act of preventing it. So the vocabulary is enforced where it applies: here,
// over `ui/src`, and nowhere else.
//
// Phone is the base and has no prefix, deliberately: a layout that has to be asked for on the
// smallest screen is one that was designed for the largest and narrowed afterwards. So this also
// checks the other direction — that the named classes are actually *used*, because a gate that only
// forbids passes just as well when every responsive rule has been deleted and the till has one
// layout again.
//
// Checked over the syntax tree rather than by grep, for the reason `token-roles.mjs` gives: the
// text `lg:` appears in prose, in this file, and inside `text-lg`, and only a parse can tell a
// class list from any of them. Run by `pnpm classes`, which `pnpm build` invokes.

import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const SRC = fileURLToPath(new URL("../src", import.meta.url));

/** Every `.tsx` under `src/`, path first. */
function sources(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = `${dir}/${entry.name}`;
    if (entry.isDirectory()) {
      return sources(path);
    }
    return entry.name.endsWith(".tsx") ? [[path, readFileSync(path, "utf8")]] : [];
  });
}

/**
 * Every literal fragment of a `class` or `classList` value.
 *
 * The same reader `token-roles.mjs` uses, and the same limit: a class assembled by a helper and
 * returned as a variable is not seen, and is not pretended to be.
 */
function classText(initializer) {
  if (ts.isStringLiteral(initializer)) {
    return initializer.text;
  }
  if (ts.isJsxExpression(initializer) && initializer.expression !== undefined) {
    const parts = [];
    const collect = (node) => {
      if (
        ts.isStringLiteral(node) ||
        ts.isNoSubstitutionTemplateLiteral(node) ||
        ts.isTemplateHead(node) ||
        ts.isTemplateMiddle(node) ||
        ts.isTemplateTail(node)
      ) {
        parts.push(node.text);
      }
      ts.forEachChild(node, collect);
    };
    collect(initializer.expression);
    return parts.join(" ");
  }
  return "";
}

/** Tailwind's stock breakpoint prefixes, which `tokens.css` clears. */
const STOCK = /(?:^|\s)(sm|md|lg|xl|2xl):/g;

/** The names this till uses instead. */
const NAMED = /(?:^|\s)(tablet|terminal):/;

const offenders = [];
let named = 0;

for (const [path, text] of sources(SRC)) {
  const source = ts.createSourceFile(path, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const walk = (node) => {
    if (ts.isJsxAttribute(node) && node.initializer !== undefined) {
      const attribute = node.name.getText();
      if (attribute === "class" || attribute === "classList") {
        const classes = classText(node.initializer);
        for (const match of classes.matchAll(STOCK)) {
          const line = source.getLineAndCharacterOfPosition(node.getStart()).line + 1;
          offenders.push(
            `${path.slice(SRC.length + 1)}:${line} uses \`${match[1]}:\` — name the device class (\`tablet:\`, \`terminal:\`) or leave it to the phone, which is the base`,
          );
        }
        if (NAMED.test(classes)) {
          named += 1;
        }
      }
    }
    ts.forEachChild(node, walk);
  };
  walk(source);
}

const failures = [...offenders];

// Five is below today's count and above zero: it fails if the responsive rules are deleted or
// quietly flattened, and does not have to be edited every time a screen grows a column count.
if (named < 5) {
  failures.push(
    `only ${named} elements name a device class — this gate forbids the stock names, and passes vacuously if nothing adapts to a device at all`,
  );
}

if (failures.length > 0) {
  console.error("device-classes: FAILED");
  for (const failure of failures) {
    console.error(`  ${failure}`);
  }
  process.exit(1);
}

console.log(
  `device-classes: ok — ${named} elements name a device class, and none names a pixel width.`,
);
