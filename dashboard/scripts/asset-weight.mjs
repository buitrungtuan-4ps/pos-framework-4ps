// The static assets the console serves are bounded, and the bound is per file (Wave 4 · PR-2).
//
// PR-2 self-hosted Noto Sans, which put a third of a megabyte of binary into a repository whose
// front end had none. That is the right trade — `unicode-range` means a reader downloads 36 kB for
// an English console and 50 kB for a Vietnamese one, and the alternative was a different typeface on
// every operating system — but it is a trade that quietly stops being right if somebody later drops
// in a CJK subset "to be safe" and adds five megabytes nobody notices until a store on a phone
// tether waits a minute for the page.
//
// So: a ceiling per file, and a ceiling on the directory. Both are stated here in bytes, beside the
// measurement they came from, rather than in a doc that can drift away from what is on disk.

import { readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const PUBLIC = fileURLToPath(new URL("../public", import.meta.url));

/** The largest a single served asset may be. `latin-ext` is 164 kB, the biggest thing here today. */
const PER_FILE_BYTES = 200 * 1024;

/** The whole directory. 331 kB today; the headroom is for one more script pack, not for a CJK face. */
const TOTAL_BYTES = 512 * 1024;

/** Every file under `public/`, with its size, deepest-first order not mattering. */
function walk(dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...walk(path));
    } else {
      out.push([path.slice(PUBLIC.length + 1), statSync(path).size]);
    }
  }
  return out;
}

let files;
try {
  files = walk(PUBLIC);
} catch {
  console.log("asset-weight: ok — no public/ directory to measure.");
  process.exit(0);
}

const kb = (bytes) => `${(bytes / 1024).toFixed(1)} kB`;
const total = files.reduce((sum, [, size]) => sum + size, 0);
const oversized = files.filter(([, size]) => size > PER_FILE_BYTES);

for (const [name, size] of files.sort((a, b) => b[1] - a[1])) {
  const flag = size > PER_FILE_BYTES ? "FAIL" : "ok  ";
  console.log(`  ${flag}  ${name}: ${kb(size)}`);
}

if (oversized.length > 0) {
  console.error(
    `\nasset-weight: ${oversized.length} file(s) over the ${kb(PER_FILE_BYTES)} ceiling.`,
  );
  process.exit(1);
}
if (total > TOTAL_BYTES) {
  console.error(`\nasset-weight: public/ is ${kb(total)}, over the ${kb(TOTAL_BYTES)} ceiling.`);
  process.exit(1);
}
console.log(
  `\nasset-weight: ok — ${files.length} file(s), ${kb(total)} of ${kb(TOTAL_BYTES)} allowed.`,
);
