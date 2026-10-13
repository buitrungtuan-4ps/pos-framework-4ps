// The ESLint size baseline (ADR-0172): code that broke a fail level when the ratchet began, with
// its figures, so `eslint.config.mjs` holds each listed file to them.
//
//   node scripts/eslint-baseline.mjs --write   measure the tree and write eslint-baseline.json
//   node scripts/eslint-baseline.mjs --check   fail if an entry is no longer needed
//
// A file is listed per rule with the largest figure it has for that rule: its line count, its
// longest function, or its deepest nesting. `pnpm lint` runs `--check` after ESLint, so an entry
// whose file is back within the limit, or is gone, must be removed, and the list only shrinks.
//
// This file is the same in `ui/` and `dashboard/`.

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join, relative } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const BASELINE = join(root, "eslint-baseline.json");
const RULES = ["max-lines", "max-lines-per-function", "max-depth"];

/** Every fail-level violation in the tree with no baseline applied: { file: { rule: figure } }. */
async function measure() {
  process.env.ESLINT_BASELINE = "off";
  const { ESLint } = await import("eslint");
  const eslint = new ESLint({ cwd: root });
  const results = await eslint.lintFiles(["."]);
  const found = {};
  for (const result of results) {
    for (const message of result.messages) {
      if (message.severity !== 2 || !RULES.includes(message.ruleId)) continue;
      const figure = Number(/\((\d+)\)/.exec(message.message)?.[1]);
      if (!Number.isFinite(figure)) throw new Error(`cannot read a figure from: ${message.message}`);
      const file = relative(root, result.filePath).split("\\").join("/");
      found[file] ??= {};
      found[file][message.ruleId] = Math.max(found[file][message.ruleId] ?? 0, figure);
    }
  }
  return found;
}

function sorted(table) {
  return Object.fromEntries(
    Object.keys(table)
      .sort()
      .map((file) => [file, Object.fromEntries(Object.keys(table[file]).sort().map((r) => [r, table[file][r]]))]),
  );
}

const mode = process.argv[2];
const found = await measure();
if (mode === "--write") {
  writeFileSync(BASELINE, `${JSON.stringify(sorted(found), null, 2)}\n`);
  console.log(`eslint-baseline: ${Object.keys(found).length} file(s) listed`);
} else if (mode === "--check") {
  const listed = JSON.parse(readFileSync(BASELINE, "utf8"));
  const stale = [];
  for (const [file, figures] of Object.entries(listed)) {
    for (const rule of Object.keys(figures)) {
      if (found[file]?.[rule] === undefined) stale.push(`${file}: ${rule}`);
    }
  }
  if (stale.length > 0) {
    console.error("eslint-baseline: these entries are within the limits now, or gone; remove them");
    console.error("(or run `node scripts/eslint-baseline.mjs --write` and check it only removes):");
    for (const entry of stale) console.error(`  ${entry}`);
    process.exit(1);
  }
  console.log(`eslint-baseline: ok, ${Object.keys(listed).length} file(s) listed`);
} else {
  console.error("usage: node scripts/eslint-baseline.mjs --write | --check");
  process.exit(2);
}
