// The no-hardcoded-strings gate (ADR-0020, AGENTS.md §2 rule 7).
//
// Parses every .tsx under src/ with the TypeScript compiler and fails on any user-visible string
// that is not routed through the i18n runtime: a JSX text node containing a letter, or a string
// literal in a user-visible attribute (placeholder, title, aria-label, alt). After extraction every
// visible string is a `{t(...)}` expression, so a bare word in the tree is a real violation. Run by
// `pnpm i18n:lint`, which the `ui` CI job invokes.

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const SRC = fileURLToPath(new URL("../src", import.meta.url));
const VISIBLE_ATTRS = new Set(["placeholder", "title", "aria-label", "alt"]);
const HAS_LETTER = /\p{L}/u;

function tsxFiles(dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...tsxFiles(path));
    } else if (entry.name.endsWith(".tsx")) {
      out.push(path);
    }
  }
  return out;
}

const violations = [];

for (const file of tsxFiles(SRC)) {
  const source = ts.createSourceFile(
    file,
    readFileSync(file, "utf8"),
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX,
  );
  const report = (node, message) => {
    const { line } = source.getLineAndCharacterOfPosition(node.getStart(source));
    violations.push(`${file}:${line + 1} ${message}`);
  };
  const walk = (node) => {
    if (ts.isJsxText(node) && HAS_LETTER.test(node.text)) {
      report(node, `hardcoded JSX text "${node.text.trim().slice(0, 40)}" — route it through t()`);
    }
    if (ts.isJsxAttribute(node) && node.initializer && ts.isStringLiteral(node.initializer)) {
      const name = node.name.getText(source);
      if (VISIBLE_ATTRS.has(name) && HAS_LETTER.test(node.initializer.text)) {
        report(node, `hardcoded ${name}="${node.initializer.text}" — route it through t()`);
      }
    }
    ts.forEachChild(node, walk);
  };
  walk(source);
}

if (violations.length > 0) {
  console.error("i18n-lint: user-visible strings must go through t() (ADR-0020):\n");
  for (const violation of violations) {
    console.error(`  ${violation}`);
  }
  console.error(`\n${violations.length} violation(s).`);
  process.exit(1);
}

console.log("i18n-lint: ok — no hardcoded user-visible strings.");
