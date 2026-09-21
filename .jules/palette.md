## 2026-08-18 - Command Palette Combobox & Modal Overlay Accessibility

**Learning:** Overlay command palettes using list filtering and keyboard navigation require explicit WAI-ARIA combobox/listbox roles (`role="dialog"`, `role="combobox"`, `role="listbox"`, `role="option"`, `aria-selected`) for screen readers. Furthermore, keyboard navigation in scrollable lists needs explicit `scrollIntoView({ block: "nearest" })` on active option updates so items don't move off-screen during arrow key traversal. Safe optional chaining (`scrollIntoView?.()`) is essential for jsdom compatibility in Vitest suite.
**Action:** Always pair `aria-selected` and active option scrolling when building keyboard-navigated search/switcher overlays.

## 2026-08-18 - WAI-ARIA Breadcrumb Navigation & Current Page Indication

**Learning:** Breadcrumb trails implemented as un-ordered elements without structured list items (`<ol>` / `<li>`) or `aria-current="page"` fail to communicate structural parent-child navigation hierarchy and current page location to screen reader users.
**Action:** Wrap breadcrumb items in semantic `<ol>` / `<li>` lists inside `<nav aria-label="...">` and mark the final item representing the active screen with `aria-current="page"`.
