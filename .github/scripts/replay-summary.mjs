// Turns a Playwright JSON report into the job summary a failed replay is read from.
//
// The job log cannot be relied on for this. A `console-replay` failure is followed by the Postgres
// service container's own output, which the runner dumps at teardown and which runs to thousands of
// lines — the failing assertion ends up far above the end of the log, and a reader (or an API) that
// looks at the tail sees database chatter rather than the test. The job summary is a separate
// surface, so it says what broke whatever the log does.
//
// Takes the report path as its one argument and writes GitHub-flavoured markdown to stdout.

import { readFileSync } from "node:fs";

const [reportPath] = process.argv.slice(2);
if (reportPath === undefined) {
  console.error("usage: replay-summary.mjs <report.json>");
  process.exit(2);
}

let report;
try {
  report = JSON.parse(readFileSync(reportPath, "utf8"));
} catch (error) {
  // A run that died before writing a report is itself the finding, so say that rather than exit
  // non-zero: this script runs *because* something already failed, and failing again would replace
  // one unexplained red with another.
  console.log(`## Replay\n`);
  console.log(`No report at \`${reportPath}\` — the run ended before writing one.\n`);
  console.log(`\`\`\`\n${String(error)}\n\`\`\`\n`);
  process.exit(0);
}

/** Every spec in the report, flattened out of its suite tree. */
function* specs(suite) {
  yield* suite.specs ?? [];
  for (const child of suite.suites ?? []) {
    yield* specs(child);
  }
}

const all = (report.suites ?? []).flatMap((suite) => [...specs(suite)]);
const failed = all.filter((spec) => spec.ok === false);

if (failed.length === 0) {
  console.log(`## Replay\n`);
  console.log(`All ${all.length} flows passed.\n`);
  process.exit(0);
}

console.log(`## Replay: ${failed.length} of ${all.length} flows failed\n`);
for (const spec of failed) {
  console.log(`### ${spec.title}\n`);
  for (const test of spec.tests ?? []) {
    for (const result of test.results ?? []) {
      for (const error of result.errors ?? []) {
        // The message carries the locator and the expectation, which is the whole diagnosis for a
        // replay: which tap did not land, or which outcome never appeared. The escape codes are
        // stripped because a summary is markdown, not a terminal.
        const message = (error.message ?? "").replaceAll(/\u001B\[[0-9;]*m/gu, "").trim();
        if (message !== "") {
          console.log(`\`\`\`\n${message}\n\`\`\`\n`);
        }
      }
    }
  }
}
