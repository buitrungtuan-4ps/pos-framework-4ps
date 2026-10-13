// ESLint's size rules for both front ends (ADR-0172): ADR-0170's TypeScript limits.
//
// Only the size rules are on. Each fail level is an `error`, and each warn level a `warn` under the
// `size/` alias, because one ESLint rule takes one severity. Blank lines and comments are not
// counted. Code that broke a fail level when the ratchet began is listed in the front end's
// `eslint-baseline.json` with its current figures, so it is held to them and cannot grow;
// `scripts/eslint-baseline.mjs` beside this directory writes that list and fails when an entry is
// no longer needed. Inline `eslint-disable` comments have no effect.
//
// This module imports nothing: it lives outside both front ends' `node_modules`, so each front
// end's `eslint.config.mjs` passes in the ESLint pieces it resolved itself.

/** Fail levels, from ADR-0172's table. */
export const FAIL = { "max-lines": 1000, "max-lines-per-function": 100, "max-depth": 4 };

const SKIP = { skipBlankLines: true, skipComments: true };

/** The options for a size rule at `max`. */
export function options(rule, max) {
  return rule === "max-depth" ? { max } : { max, ...SKIP };
}

/**
 * The flat config for one front end.
 *
 * @param {object} deps
 * @param {Map<string, object>} deps.builtinRules  `eslint/use-at-your-own-risk`'s `builtinRules`
 * @param {object} deps.parser                     `typescript-eslint`'s parser
 * @param {object} deps.baseline                   the front end's `eslint-baseline.json`
 */
export function sizeConfig({ builtinRules, parser, baseline }) {
  // `ESLINT_BASELINE=off` measures the tree without the baseline, which is how the script writes it.
  const listed = process.env.ESLINT_BASELINE === "off" ? {} : baseline;
  return [
    { ignores: ["dist/**", "node_modules/**", "test-results/**", "playwright-report/**", "tests/**", "bench/**"] },
    {
      files: ["**/*.{ts,tsx,mjs,js}"],
      languageOptions: {
        parser,
        ecmaVersion: "latest",
        sourceType: "module",
        parserOptions: { ecmaFeatures: { jsx: true } },
      },
      linterOptions: { noInlineConfig: true, reportUnusedDisableDirectives: "error" },
      plugins: {
        size: {
          rules: {
            "max-lines": builtinRules.get("max-lines"),
            "max-lines-per-function": builtinRules.get("max-lines-per-function"),
          },
        },
      },
      rules: {
        "max-lines": ["error", options("max-lines", FAIL["max-lines"])],
        "size/max-lines": ["warn", options("max-lines", 500)],
        "max-lines-per-function": ["error", options("max-lines-per-function", FAIL["max-lines-per-function"])],
        "size/max-lines-per-function": ["warn", options("max-lines-per-function", 50)],
        complexity: ["warn", 10],
        "max-depth": ["error", options("max-depth", FAIL["max-depth"])],
        "max-params": ["warn", 5],
      },
    },
    ...Object.entries(listed).map(([file, figures]) => ({
      files: [file],
      rules: Object.fromEntries(
        Object.entries(figures).map(([rule, max]) => [rule, ["error", options(rule, max)]]),
      ),
    })),
  ];
}
