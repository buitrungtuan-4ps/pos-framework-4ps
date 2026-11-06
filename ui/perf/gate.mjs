// The till's 10 ms tap budget, on every pull request (ADR-0208).
//
// Runs the tap-to-paint harness (`perf/tap-to-paint.spec.mjs`) at a CPU throttle of 2x
// (`POS_PERF_CPU_THROTTLE`, Chrome DevTools Protocol `Emulation.setCPUThrottlingRate`; 2x is a
// stand-in until a reference NUC calibrates it) and holds each act to (decision 3):
//
// - at least 100 samples;
// - `frameReady` at the median at most 10 ms: the tap to the frame that answers it being ready to
//   paint, the wait for the display left out (decision 1).
//
// Each act's p95, and how many of its samples made the till miss a frame (decision 2), are printed
// and fail nothing: they are recorded until phase 9 tightens the gate on a reference NUC. A shared
// runner is noisy, so an act that fails is measured once more, in a second run, and the gate fails
// only for an act that fails both. The reload is a reload, not a tap, and keeps ADR-0173's
// budget, which the nightly run enforces. `just perf-gate` builds what it needs and runs this.

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";

const BUDGET_MS = 10;
const MIN_SAMPLES = 100;
const THROTTLE = process.env["POS_PERF_CPU_THROTTLE"] ?? "2";
// Enough rounds that the act sampled least, moving a table (about five a round), reaches 100.
const ROUNDS = process.env["POS_PERF_ROUNDS"] ?? "20";
const SEATS = process.env["POS_PERF_SEATS"] ?? "5";

/** One run of the harness; its figures per act. */
function run(label) {
  const out = `test-results/perf-gate-${label}.json`;
  const result = spawnSync("pnpm", ["exec", "playwright", "test", "--config", "playwright.perf.config.mjs"], {
    stdio: ["ignore", "ignore", "inherit"],
    env: {
      ...process.env,
      POS_PERF_CPU_THROTTLE: THROTTLE,
      POS_PERF_ROUNDS: ROUNDS,
      POS_PERF_SEATS: SEATS,
      POS_PERF_OUT: out,
      POS_PERF_ENFORCE: "0",
    },
  });
  if (result.status !== 0) {
    console.error(`perf-gate: the harness itself failed (run ${label}), so nothing was measured`);
    process.exit(2);
  }
  return JSON.parse(readFileSync(out, "utf8")).frameReady;
}

/** Why an act's figures break the budget, or `null`. The p95 and long frames are recorded only. */
function breach(figures) {
  if (figures.n < MIN_SAMPLES) return `${figures.n} samples, under ${MIN_SAMPLES}`;
  if (figures.p50 > BUDGET_MS) return `p50 ${figures.p50} ms over ${BUDGET_MS} ms`;
  return null;
}

function report(label, acts) {
  for (const [act, figures] of Object.entries(acts)) {
    const why = breach(figures);
    console.log(
      `${label}  ${act.padEnd(24)} n ${String(figures.n).padStart(4)}  p50 ${figures.p50}  (p95 ${figures.p95}  long ${figures.longFrames}, recorded)  ${why ?? "ok"}`,
    );
  }
}

const first = run("1");
report("run 1", first);
const failing = Object.keys(first).filter((act) => breach(first[act]) !== null);
if (failing.length === 0) {
  console.log(`perf-gate: every act's p50 within ${BUDGET_MS} ms at ${THROTTLE}x`);
  process.exit(0);
}
console.log(`perf-gate: ${failing.join(", ")} over budget; measuring once more`);
const second = run("2");
report("run 2", second);
const failed = failing.filter((act) => second[act] === undefined || breach(second[act]) !== null);
if (failed.length === 0) {
  console.log(`perf-gate: every act's p50 within ${BUDGET_MS} ms at ${THROTTLE}x on the second run`);
  process.exit(0);
}
console.error(`perf-gate: p50 over the ${BUDGET_MS} ms budget at ${THROTTLE}x in both runs: ${failed.join(", ")} (ADR-0208)`);
process.exit(1);
