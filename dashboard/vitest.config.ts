// The console's test runner (Wave 3, Stage 0).
//
// Separate from `vite.config.ts` on purpose. That file describes how the console is *built and
// served* — Tailwind, the `dist/` output, the dev proxy — and none of it is wanted here; a
// behavioural test renders a component into jsdom and asserts what happens, so all it needs is the
// Solid transform. Keeping the two apart also means a test-only setting can never change what
// ships.
//
// `resolve.conditions` is the one non-obvious line: Solid publishes a *server* build and a
// *browser* build under export conditions, and without naming the browser one here Vitest resolves
// the server build, whose `render` writes strings instead of DOM nodes. The failure that produces is
// deeply unhelpful, so it is pinned rather than discovered.
//
// Vitest's globals stay off. `describe`/`it`/`expect` are imported explicitly in every test file:
// `verbatimModuleSyntax` is on in this project and an explicit import is what `tsc --noEmit` can
// check, whereas ambient globals need a types entry that then applies to the whole `src/` tree too.

import { defineConfig } from "vitest/config";
import solid from "vite-plugin-solid";

export default defineConfig({
  // `hot: false` matters: the plugin's hot-reload wrapper calls a component through `untrack` from
  // outside the render tree, which makes `useNavigate` throw "router primitives can be only used
  // inside a Route" for any component that reads the router. There is nothing to hot-reload in a
  // single test run anyway.
  plugins: [solid({ hot: false })],
  resolve: {
    conditions: ["development", "browser"],
  },
  test: {
    // `tests/`, deliberately not `src/`. All four of the shipped front-end gates — the
    // no-hardcoded-strings lint, the i18n parity check, the WCAG contrast audit and the step budget
    // — walk `src/`, and a test file legitimately contains bare English ("4P's Ben Thanh", the text
    // an assertion looks for). Putting tests outside `src/` keeps every one of those gates honest
    // without teaching four scripts about a fifth file naming convention.
    include: ["tests/**/*.test.{ts,tsx}"],
    environment: "jsdom",
    restoreMocks: true,
  },
});
