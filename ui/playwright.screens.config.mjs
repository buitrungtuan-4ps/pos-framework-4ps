// The screen walk ([`tests/screens.spec.mjs`](tests/screens.spec.mjs)), which is a **record** and not
// a gate.
//
// It has its own config because it must not run inside `pnpm replay`. The step gate answers "does
// every declared flow still work"; this answers "what does an operator actually see", photographs 17
// screens at 3 device sizes, and takes minutes to do it. Putting the two in one project would make
// every pull request pay for a record nobody reads on that run, and would let a slow walk fail the
// gate for a reason that has nothing to do with the change.
//
// Run it with `pnpm screens`. The base config ignores the file, so neither invocation can pick up
// the other's tests.

import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  testMatch: ["**/screens.spec.mjs"],
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"]],
  // Each of the three walks boots its own edge and drives 17 states through it. The per-step budget
  // that keeps a stuck modal from eating the whole run lives in the spec (`STEP_MS`); this is only
  // the outer bound on one device's walk.
  timeout: 300_000,
  use: {
    ...devices["Desktop Chrome"],
    headless: true,
    ...(process.env["POS_UI_CHROMIUM"] === undefined
      ? {}
      : { launchOptions: { executablePath: process.env["POS_UI_CHROMIUM"] } }),
    trace: "off",
    video: "off",
  },
});
