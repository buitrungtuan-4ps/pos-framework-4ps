// The tap-to-paint measurement ([`perf/tap-to-paint.spec.mjs`](perf/tap-to-paint.spec.mjs)), which
// is a **record** and not a gate, as the screen walk is.
//
// It has its own config and its own directory so that `pnpm replay` never runs it: ADR-0109 keeps
// timing out of every commit's checks, because a number from a shared runner on a debug build is a
// number about the runner. ADR-0173 decision 6 runs it nightly, against a release-built edge and a
// production build of the till.
//
// Run it with `pnpm perf`, with `POS_EDGE_BIN` pointing at a release `minimal-edge`.

import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./perf",
  testMatch: ["**/*.spec.mjs"],
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"]],
  // Several rounds, each booting its own edge and making a few hundred taps.
  timeout: 600_000,
  expect: { timeout: 10_000 },
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
