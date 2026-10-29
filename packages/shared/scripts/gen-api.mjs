// Generates a front end's API types from a committed OpenAPI document (ADR-0177 decision 6).
//
//   node ../packages/shared/scripts/gen-api.mjs <document.json> <output.d.ts>
//
// Run by `pnpm gen:api` from `ui/` (the till's `docs/openapi-edge.json`) and `dashboard/` (the
// console's `docs/openapi-admin.json`), with paths relative to the front end's own root.
//
// The output's first line names the git blob id of the document it came from, computed here the way
// `git hash-object` computes it. `cargo xtask api-types --check` compares that id with the committed
// document's, so a document regenerated without its types fails `just preflight` with no Node in the
// gate. `tsc`, already in each app's build, proves the types compile.

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";

const [input, output] = process.argv.slice(2);
if (!input || !output) {
  console.error("usage: gen-api.mjs <document.json> <output.d.ts>");
  process.exit(2);
}

const document = readFileSync(input);
// What `git hash-object` prints: the SHA-1 of "blob <size>\0" and the bytes.
const blob = createHash("sha1")
  .update(`blob ${document.length}\0`)
  .update(document)
  .digest("hex");

const repository = resolve(dirname(new URL(import.meta.url).pathname), "../../..");
const named = relative(repository, resolve(input)).split("\\").join("/");

// The CLI, from this front end's own `node_modules`, so each app uses the version it pins.
const cli = join(process.cwd(), "node_modules", ".bin", "openapi-typescript");
const staged = `${output}.staged`;
mkdirSync(dirname(output), { recursive: true });
execFileSync(cli, [input, "--output", staged], { stdio: ["ignore", "ignore", "inherit"] });
const types = readFileSync(staged, "utf8");
rmSync(staged);

writeFileSync(
  output,
  `// @generated from ${named}, git blob ${blob}, by \`pnpm gen:api\` (ADR-0177). Do not edit.\n${types}`,
);
console.log(`wrote ${output} from ${named} (${blob})`);
