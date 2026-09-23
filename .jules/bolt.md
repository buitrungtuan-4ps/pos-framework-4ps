## 2026-09-23 - A Test-Environment Workaround Is Not Part Of The Optimization

**Learning:** The Releases memoization was sound on its own. What followed it was a third commit to an unrelated shared component — `ComboboxField` in `ui.tsx` — wrapping its click-outside listener registration in `setTimeout(…, 0)`, commented "can interfere with Playwright click actions in test environments". That changes what every picker in the console does at runtime, to settle a complaint about the harness, and it contradicts the comment directly above it explaining why the listener binds on `pointerdown` in the first place. It was also unnecessary: the console replay suite passes 9/9 with the Releases change alone, including `a picker whose options arrive late does not swallow the next click` — the very test the delay was aimed at.

**Action:** When a change outside the stated purpose appears late in a branch to make CI pass, treat it as a finding to verify, not a fix to keep. Run the suite without it first. If it really is needed, the failure it hides is the actual bug and belongs in its own change with its own reasoning — never appended to an unrelated optimization.

## 2026-09-23 - Fold Both Sides in the Caller's Memos, Not Inside the Comparison

**Learning:** `matches(query, captions)` folded both sides inside the per-item scan, so a keystroke cost ~450 NFD passes over strings that had not changed. The fix is not to cache inside `fold` — a module-level last-input/last-output pair makes a pure string helper stateful and only ever saves the *query*, because the captions differ from each other on every call. It is to move both folds to where a memo already exists: captions fold when the menu changes, the query folds once per keystroke, and the comparison receives strings that are already folded. One fold per keystroke instead of one per item, with no state added anywhere.

**Action:** When a helper is called once per item of a scan, look for the memo that already brackets the scan and hoist the invariant work into it, rather than caching inside the helper. And prefer one folded array to a raw/folded pair: two parallel arguments that must agree are a way for them to disagree.

## 2026-09-17 - Ordering createMemo Declarations to Prevent TDZ Errors in SolidJS

**Learning:** In SolidJS, `createMemo` evaluates immediately upon component initialization. Defining `createMemo` accessors above the signal/resource getters they reference (e.g. `countries()`) causes a Temporal Dead Zone error (`ReferenceError: Cannot access ... before initialization`) when mounted. Placing `createMemo` declarations below their dependent accessor definitions prevents TDZ runtime failures while maintaining cached performance benefits.

**Action:** Always place `createMemo` declarations below the signals, resources, and derived accessor functions they depend on.

## 2026-04-01 - Memoizing Keys & Locale Completion Map in SolidJS Translations Editor

**Learning:** In SolidJS table components rendering matrix-like translation grids ($K$ keys $\times$ $L$ locales), un-memoized getters for key sorting (`Object.keys().sort()`) and per-locale completion calculations (`keys().filter(...)` per header column) re-execute $O(L \cdot K)$ string checks on every input stroke or re-render. Pre-building a `completionMap` with `createMemo` in a single $O(K \cdot L)$ pass and memoizing sorted `keys` prevents redundant array sorting/filtering and removes render stutter on large grids.

**Action:** In SolidJS table headers with aggregated stats per column, compute all column statistics in a single `createMemo` lookup map instead of calling $O(K)$ array filter functions inside JSX `<For each={columns}>`.

## 2026-04-01 - Pre-grouping placements in Rust catalog compilation

**Learning:** When resolving hierarchical inheritance chains (e.g. menu overrides), linear scanning of placements for each node in the inheritance chain causes $O(\text{chain\_depth} \times \text{placements})$ iterations. Pre-grouping placements by `menu_id` into a lookup map reduces iteration overhead to $O(\text{placements} \log \text{menus} + \text{chain\_depth})$.

**Action:** When walking parent/child hierarchy chains over relational collections in Rust, pre-group child items by parent key into a BTreeMap/HashMap prior to the hierarchy traversal.

## 2026-03-31 - Memoizing Grid Cell Map in SolidJS Layout Editor

**Learning:** In SolidJS, un-memoized getters that compute matrix cells via nested loop filters (`for row ... for col ... array.filter(...)`) re-evaluate on every access/re-render, causing $O(R \cdot C \cdot N)$ runtime complexity (~20,000 iterations for 20x20 grid with 50 items). Using `createMemo` with an $O(N)$ Map grouping cuts iteration count to $O(N + R \cdot C)$ (~450 operations).

**Action:** Whenever generating 2D grid/matrix data from linear arrays in SolidJS components, pre-group elements into a Hash Map keyed by coordinate before building the grid matrix, and wrap with `createMemo`.

## 2026-03-26 - Fast-path for static i18n translations
**Learning:** Over 90% of i18n keys in `ui` and `dashboard` are static strings. Calling `IntlMessageFormat.format()` even with compiled/cached formatters incurs unnecessary AST formatting overhead. Fast-pathing static strings without `{` and without `args` reduces translation lookup time by ~10x (~0.5ms vs ~0.05ms per 1k lookups).
**Action:** When working with `intl-messageformat` i18n lookups, check if the string is static and has no ICU tags before passing it to `format()`.
