// The till's unit-test runner (gate A5): the same Vitest the console runs, for the modules with no
// screen in them, which nothing but a reviewer's eyes checked before.
//
// Separate from `vite.config.ts` for the console's reason: that file says how the till is built and
// served, and a test-only setting must never change what ships. `resolve.conditions` names Solid's
// browser build, without which Vitest resolves the server build (`dashboard/vitest.config.ts` has the
// long story).
//
// The tests live in `unit/`, not `tests/`. `tests/` is Playwright's directory, and its default match
// takes any `*.test.ts` there as a browser test; `unit/` also keeps them out of `src/`, which the
// no-hardcoded-strings lint and the other shipped gates walk.

import { defineConfig } from "vitest/config";
import solid from "vite-plugin-solid";

export default defineConfig({
  plugins: [solid({ hot: false })],
  resolve: {
    conditions: ["development", "browser"],
  },
  test: {
    include: ["unit/**/*.test.ts"],
    environment: "jsdom",
    restoreMocks: true,
  },
});
