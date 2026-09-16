// The one rule `tokens.css` states in prose and nothing measured: the accent is never on a button.
//
// `--primary` and `--accent` were split in Wave 4 (D1) so that a fork sets its brand in one token
// and repaints nothing else. The token file says so at the declaration — "`--accent` … is used
// where brand belongs — the logo, the focus ring, the selected nav tint, a link — never on a
// button" — and for a wave the till had thirteen buttons painted `bg-accent`, including the key
// that takes money. Both statements were in the same repository and only one of them was enforced.
//
// So this gate reads what the rule is about: a `<button>`, and the class list it carries. A button
// may not paint itself in the accent or wear the accent's ink; everything else the accent does —
// `text-accent` on a link, `border-accent` on the tender the operator has chosen — is untouched,
// because a border and a fill are different claims about whose colour that control is.
//
// Checked over the syntax tree rather than by grep: `bg-accent` in a file says nothing about which
// element wears it, and the selected-state borders sit two lines from the buttons. Run by
// `pnpm tokens`, which `pnpm build` invokes, so the `ui` CI job fails on a breach.
//
// The pair of assertions matters as much as the first one. A gate that only forbids passes just as
// well when every primary button has been deleted, so the count below also requires the till to
// still have primary buttons to get wrong.

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
 * Every literal fragment of a `class` value, whichever spelling it uses.
 *
 * `class="…"`, `class={`…`}` and the conditional inside a template all end up here as plain text.
 * A class assembled by a helper and returned as a variable is not seen, and is not pretended to be:
 * the till writes its classes at the element.
 */
function classText(initializer) {
  if (ts.isStringLiteral(initializer)) {
    return initializer.text;
  }
  if (ts.isJsxExpression(initializer) && initializer.expression !== undefined) {
    const parts = [];
    const collect = (node) => {
      if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node) || ts.isTemplateHead(node) || ts.isTemplateMiddle(node) || ts.isTemplateTail(node)) {
        parts.push(node.text);
      }
      ts.forEachChild(node, collect);
    };
    collect(initializer.expression);
    return parts.join(" ");
  }
  return "";
}

/** The accent wearing a button's two roles: the fill, and the ink that only exists to sit on it. */
const ACCENT_ON_A_BUTTON = /\bbg-accent\b|\btext-accent-ink\b/;
const PRIMARY = /\bbg-primary\b/;

const offenders = [];
let primaries = 0;

for (const [path, text] of sources(SRC)) {
  const source = ts.createSourceFile(path, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const walk = (node) => {
    if (
      (ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node)) &&
      node.tagName.getText() === "button"
    ) {
      for (const attribute of node.attributes.properties) {
        if (!ts.isJsxAttribute(attribute) || attribute.initializer === undefined) {
          continue;
        }
        if (attribute.name.getText() !== "class") {
          continue;
        }
        const classes = classText(attribute.initializer);
        if (ACCENT_ON_A_BUTTON.test(classes)) {
          const line = source.getLineAndCharacterOfPosition(node.getStart()).line + 1;
          offenders.push(`${path.slice(SRC.length + 1)}:${line}`);
        }
        if (PRIMARY.test(classes)) {
          primaries += 1;
        }
      }
    }
    ts.forEachChild(node, walk);
  };
  walk(source);
}

const failures = offenders.map(
  (site) =>
    `${site}: a button painted in the accent — \`--accent\` is the fork's brand, and src/styles/tokens.css reserves it for the logo, the focus ring, the selected tint and links. A button that means "do the thing" takes \`bg-primary\`/\`text-primary-ink\`.`,
);

// Ten is below today's thirteen and above zero: it fails if the primary buttons are deleted or
// quietly repainted, and does not have to be edited every time a screen grows a button.
if (primaries < 10) {
  failures.push(
    `only ${primaries} buttons take \`bg-primary\` — this gate forbids the accent on a button, and passes vacuously if there are no primary buttons left to check`,
  );
}

if (failures.length > 0) {
  console.error("token-roles: FAILED");
  for (const failure of failures) {
    console.error(`  ${failure}`);
  }
  process.exit(1);
}

console.log(
  `token-roles: ok — ${primaries} primary buttons, and the accent is on none of them.`,
);
