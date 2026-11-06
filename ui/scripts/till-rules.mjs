// Builds the till's rules module (`crates/pos-till-rules`, ADR-0193 decision 2, ADR-0202 decision 1)
// for `wasm32-unknown-unknown` and puts it where the till's build takes it from,
// `src/rules/pos_till_rules.wasm`. `pnpm build` runs this first, so the module a till loads is
// always built from the same commit as the till. The file is a build output and is not committed.
//
// Needs the repository's pinned Rust toolchain, which lists the `wasm32-unknown-unknown` target
// (`rust-toolchain.toml`). `CARGO_TARGET_DIR` is honoured.

import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";

const ui = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const root = resolve(ui, "..");
// Cargo runs from the repository root, so a relative target directory is read from there too.
const target = resolve(root, process.env["CARGO_TARGET_DIR"] ?? "target");
const built = join(target, "wasm32-unknown-unknown", "till-rules", "pos_till_rules.wasm");
const out = join(ui, "src", "rules", "pos_till_rules.wasm");

/** The module's size, gzipped, it may not grow past (ADR-0193 decision 2). */
const GZIP_BYTES_MAX = 400_000;

const build = spawnSync(
  "cargo",
  ["build", "--locked", "-p", "pos-till-rules", "--target", "wasm32-unknown-unknown", "--profile", "till-rules"],
  { cwd: root, stdio: "inherit" },
);
if (build.status !== 0) {
  console.error("till-rules: the rules module did not build");
  process.exit(build.status ?? 1);
}
mkdirSync(dirname(out), { recursive: true });
copyFileSync(built, out);
const gzipped = gzipSync(readFileSync(out), { level: 9 }).length;
console.log(`till-rules: ${statSync(out).size} bytes, ${gzipped} gzipped`);
if (gzipped > GZIP_BYTES_MAX) {
  console.error(`till-rules: the module is over ${GZIP_BYTES_MAX} bytes gzipped (ADR-0193 decision 2)`);
  process.exit(1);
}
