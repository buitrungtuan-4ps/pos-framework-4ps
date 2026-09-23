## 2026-09-23 - The Fourth Copy Is The Signal To Extract, Not To Write

**Learning:** Click-outside dismissal was missing from the three header dropdowns and worth adding. But the same fourteen-line effect already existed in `ComboboxField`, so inlining it three more times makes four copies of one idea — and they had already begun to disagree, the new ones calling `setTimeout(() => setOpen(false), 0)` where the original calls `close()` directly, with nothing to say why. `lib/escape.ts` is the repository's own precedent sitting one line above each of those popovers: the same behaviour for the keyboard, extracted to `lib/` for exactly this reason, with a header explaining that a helper only five files can see is a helper everyone else goes without.

**Action:** Before adding a behaviour to several components, grep for it — a copy already in the tree makes yours the extraction, not the addition. And when the original has no test, write one at that site first: it is what proves the extraction preserved the behaviour, and its absence is why the copy could drift in the first place.

## 2026-09-23 - Focus On Open Must Wait For The Element, Not Only For The Panel

**Learning:** Focusing a search box from an effect that watches only `open()` works from the second open onward and never on the first. On the first open the list behind the box is still loading, the `<Show>` is rendering its skeleton, and the ref is still `undefined` — so `queueMicrotask(() => ref?.focus())` focuses nothing and the effect never runs again, because nothing it tracked changed. The optional-chain that makes it safe is also what makes it silent.

**Action:** An effect that focuses a conditionally-rendered element must read the signal that decides whether it is rendered, so it re-runs when the element appears; and latch it to once per opening, or a list refresh while the panel is open will haul the cursor out of whatever the operator is typing. Test the **first** open — a test written against a reopen passes against exactly this bug.

## 2026-08-18 - Command Palette Combobox & Modal Overlay Accessibility

**Learning:** Overlay command palettes using list filtering and keyboard navigation require explicit WAI-ARIA combobox/listbox roles (`role="dialog"`, `role="combobox"`, `role="listbox"`, `role="option"`, `aria-selected`) for screen readers. Furthermore, keyboard navigation in scrollable lists needs explicit `scrollIntoView({ block: "nearest" })` on active option updates so items don't move off-screen during arrow key traversal. Safe optional chaining (`scrollIntoView?.()`) is essential for jsdom compatibility in Vitest suite.
**Action:** Always pair `aria-selected` and active option scrolling when building keyboard-navigated search/switcher overlays.

## 2026-08-18 - WAI-ARIA Breadcrumb Navigation & Current Page Indication

**Learning:** Breadcrumb trails implemented as un-ordered elements without structured list items (`<ol>` / `<li>`) or `aria-current="page"` fail to communicate structural parent-child navigation hierarchy and current page location to screen reader users.
**Action:** Wrap breadcrumb items in semantic `<ol>` / `<li>` lists inside `<nav aria-label="...">` and mark the final item representing the active screen with `aria-current="page"`.
