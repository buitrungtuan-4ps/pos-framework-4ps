// The browser half of the console's step gate
// ([ADR-0109](../docs/adr/0109-counting-the-taps-an-operator-makes.md)).
//
// One project, headless Chromium, one worker. The run shares a single `pos_cloud` and a single
// seeded tenant, so the flows must not interleave — and with one worker that is a fact rather than
// a hope. There is no `webServer` here on purpose: the harness starts the cloud itself, because it
// also makes the database, enrols the first admin and reads the TOTP secret back
// (`tests/cloud.mjs`), none of which a command line could hand it.
//
// No retries. A replay that passes on the second attempt is telling you something about the flow,
// and a gate that hides it is the gate this whole record argues against.

import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  testMatch: "replay.spec.mjs",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  forbidOnly: Boolean(process.env["CI"]),
  // `list` for a person watching the run; `json` so a failure can be read back without scrolling
  // — CI turns it into the job summary, where it survives whatever else the log carries.
  reporter: [["list"], ["json", { outputFile: "test-results/report.json" }]],
  // Generous, because the first test waits for the cloud to boot and migrate. It is not a
  // performance assertion: ADR-0109 deliberately asserts no timing, because "fast enough" on a
  // shared runner is a number about the runner.
  timeout: 120_000,
  expect: { timeout: 15_000 },
  use: {
    ...devices["Desktop Chrome"],
    headless: true,
    // Normally Playwright brings its own browser (`playwright install chromium`, which the CI job
    // runs). `POS_UI_CHROMIUM` points it at one already on the machine instead — an air-gapped
    // runner, a distro package, a sandbox with the download blocked. The till's config reads the
    // same variable, so a machine set up for one harness is set up for both.
    ...(process.env["POS_UI_CHROMIUM"] === undefined
      ? {}
      : { launchOptions: { executablePath: process.env["POS_UI_CHROMIUM"] } }),
    // On a failure the trace is the difference between "a click did not land" and knowing which.
    trace: "retain-on-failure",
    video: "off",
  },
});
