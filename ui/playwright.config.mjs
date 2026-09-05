// The browser half of the step gate ([ADR-0109](../docs/adr/0109-counting-the-taps-an-operator-makes.md)).
//
// One project, headless Chromium, one worker: each test starts its own `examples/minimal-edge` and
// sells against it, so the flows must not interleave on a shared store. There is no `webServer` here
// on purpose — the harness starts the edge itself, because the pairing code and the demo badge are
// read out of the process's own output rather than written down a second time (`tests/edge.mjs`).
//
// No retries. A replay that passes on the second attempt is telling you something about the flow,
// and a gate that hides it is the gate the whole record argues against.

import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  forbidOnly: Boolean(process.env["CI"]),
  reporter: [["list"]],
  // Generous, because each test boots a fresh edge (a second or two) before it taps anything. It is
  // not a performance assertion: ADR-0109 deliberately asserts no timing, because "fast enough" on a
  // shared runner is a number about the runner.
  timeout: 60_000,
  expect: { timeout: 10_000 },
  use: {
    ...devices["Desktop Chrome"],
    headless: true,
    // Normally Playwright brings its own browser (`playwright install chromium`, which the CI job
    // runs). `POS_UI_CHROMIUM` points it at one that is already on the machine instead — an
    // air-gapped runner, a distro package, a sandbox with the download blocked. Unset everywhere it
    // is not needed, so the default path stays the ordinary one.
    ...(process.env["POS_UI_CHROMIUM"] === undefined
      ? {}
      : { launchOptions: { executablePath: process.env["POS_UI_CHROMIUM"] } }),
    // On a failure the trace is the difference between "a tap did not land" and knowing which.
    trace: "retain-on-failure",
    video: "off",
  },
});
