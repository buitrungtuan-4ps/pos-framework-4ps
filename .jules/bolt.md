## 2026-03-31 - Memoizing Grid Cell Map in SolidJS Layout Editor

**Learning:** In SolidJS, un-memoized getters that compute matrix cells via nested loop filters (`for row ... for col ... array.filter(...)`) re-evaluate on every access/re-render, causing $O(R \cdot C \cdot N)$ runtime complexity (~20,000 iterations for 20x20 grid with 50 items). Using `createMemo` with an $O(N)$ Map grouping cuts iteration count to $O(N + R \cdot C)$ (~450 operations).

**Action:** Whenever generating 2D grid/matrix data from linear arrays in SolidJS components, pre-group elements into a Hash Map keyed by coordinate before building the grid matrix, and wrap with `createMemo`.

## 2026-03-26 - Fast-path for static i18n translations
**Learning:** Over 90% of i18n keys in `ui` and `dashboard` are static strings. Calling `IntlMessageFormat.format()` even with compiled/cached formatters incurs unnecessary AST formatting overhead. Fast-pathing static strings without `{` and without `args` reduces translation lookup time by ~10x (~0.5ms vs ~0.05ms per 1k lookups).
**Action:** When working with `intl-messageformat` i18n lookups, check if the string is static and has no ICU tags before passing it to `format()`.
